//! OpenAI Responses API public ingress.
//!
//! The complete request and upstream response stay as JSON values so current
//! and future Responses fields are never narrowed to KeyCompute's common chat
//! representation. A small, redacted projection is used only for routing,
//! token estimation, tracing, and billing.

use crate::{
    error::{ApiError, Result},
    extractors::{AuthExtractor, ClientRequestId, RequestId, RequestReceivedAt},
    middleware::{
        enforce_authenticated_tpm_limit, sanitize_openai_responses_error_code,
        sanitize_openai_responses_error_param, sanitize_openai_responses_error_type,
    },
    state::{
        AppState, RESPONSES_LARGE_HTTP_BODY_BYTES, ResponsesAffinity, ResponsesHttpBodyPermit,
    },
};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, RawQuery, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
};
use futures::{Stream, StreamExt};
use keycompute_auth::Permission;
use keycompute_db::models::{
    account::Account,
    response_affinity::ResponseAffinity,
    responses_idempotency_claim::{
        RESPONSES_IDEMPOTENCY_MAX_IDENTITIES_PER_TENANT,
        RESPONSES_IDEMPOTENCY_MAX_RESPONSE_BODY_BYTES, ResponsesIdempotencyClaim,
        ResponsesIdempotencyClaimMetadata,
    },
};
#[cfg(test)]
use keycompute_ratelimit::RateLimitKey;
use keycompute_types::{
    AccountApiCapability, ClientResponseOutcome, ClientUpstreamResponse, ErrorOrigin,
    ExecutionPlan, ExecutionTarget, Message, MessageContent, MessageRole,
    NoopRequestLifecycleRecorder, RequestContext, RequestLifecycleRecorder, RequestStatus,
    RequestTraceStart, RouteType, TraceErrorCategory,
};
use llm_gateway::{JsonRequestMethod, PassthroughBody};
use llm_protocol_provider::{
    ByteStream, LARGE_JSON_BODY_ADMISSION_BYTES, LARGE_JSON_WORKING_SET_ADMISSION_BYTES,
    LargeBodyPermit, MAX_JSON_PASSTHROUGH_BODY_BYTES, MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES,
    NativeStreamEvent, ProtocolType, StreamEvent, estimated_json_parse_working_set_bytes,
    try_acquire_large_body_permit,
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use sea_orm::TransactionTrait;
use serde::{
    Deserialize, Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    convert::Infallible,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

/// OpenAI inline skills currently allow a 70,254,592-character base64 payload.
/// Leave room for the surrounding JSON request while keeping a hard bound.
pub(crate) const OPENAI_RESPONSES_BODY_LIMIT_BYTES: usize = 80 * 1024 * 1024;
/// Bound the simultaneous request text and `serde_json::Value` allocation. The
/// limit still admits the official inline-skill payload while rejecting JSON
/// with a pathological number of tiny containers before deserialization.
pub(crate) const OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES: usize = 192 * 1024 * 1024;
const OPENAI_RESPONSES_MIN_MAX_OUTPUT_TOKENS: u32 = 16;
const RESPONSES_SETTLEMENT_CONCURRENCY: usize = 16;
const RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES: usize = 8;
const RESPONSES_CONVERSATION_DISCOVERY_MAX_DURATION: Duration = Duration::from_secs(30);
const RESPONSES_IDEMPOTENCY_REPLAY_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const RESPONSES_IDEMPOTENCY_REPLAYS_PER_TENANT: u64 = 1024;
const RESPONSES_IDEMPOTENCY_REPLAY_BYTES_PER_TENANT: u64 = 128 * 1024 * 1024;
const RESPONSES_JSON_PROCESSING_CAPACITY_MESSAGE: &str =
    "Responses JSON processing capacity is exhausted";
const _: () = assert!(
    RESPONSES_IDEMPOTENCY_MAX_RESPONSE_BODY_BYTES == MAX_JSON_PASSTHROUGH_BODY_BYTES as u64
);
const STORED_WARMUPS_PER_TENANT: u64 = 128;
const STORED_WARMUP_BYTES_PER_TENANT: u64 = 512 * 1024 * 1024;

struct ResponsesRoutingFields {
    model: String,
    stream: bool,
    max_output_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    messages: Vec<Message>,
}

impl ResponsesRoutingFields {
    fn parse(body: &Value) -> Result<Self> {
        Self::parse_fields(body)
    }

    fn parse_input_tokens(body: &Value) -> Result<Self> {
        Self::parse_fields(body)
    }

    fn parse_fields(body: &Value) -> Result<Self> {
        let object = validate_responses_reference_fields(body)?;
        let model = match object.get("model") {
            Some(Value::String(model)) if !model.trim().is_empty() => model.clone(),
            None | Some(Value::Null) => String::new(),
            _ => {
                return Err(ApiError::BadRequest(
                    "model must be a non-empty string".to_string(),
                ));
            }
        };
        let stream = match object.get("stream") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(stream)) => *stream,
            Some(_) => {
                return Err(ApiError::BadRequest(
                    "stream must be a boolean or null".to_string(),
                ));
            }
        };
        let max_output_tokens = optional_u32(object.get("max_output_tokens"), "max_output_tokens")?;
        if max_output_tokens.is_some_and(|tokens| tokens < OPENAI_RESPONSES_MIN_MAX_OUTPUT_TOKENS) {
            return Err(ApiError::BadRequest(format!(
                "max_output_tokens must be at least {OPENAI_RESPONSES_MIN_MAX_OUTPUT_TOKENS}"
            )));
        }
        let temperature = optional_f32(object.get("temperature"), "temperature")?;
        if let Some(value) = temperature
            && !(0.0..=2.0).contains(&value)
        {
            return Err(ApiError::BadRequest(
                "temperature must be between 0.0 and 2.0".to_string(),
            ));
        }
        let top_p = optional_f32(object.get("top_p"), "top_p")?;
        if let Some(value) = top_p
            && !(0.0..=1.0).contains(&value)
        {
            return Err(ApiError::BadRequest(
                "top_p must be between 0.0 and 1.0".to_string(),
            ));
        }

        Ok(Self {
            model,
            stream,
            max_output_tokens,
            temperature,
            top_p,
            messages: context_messages(body),
        })
    }
}

fn validate_responses_reference_fields(body: &Value) -> Result<&serde_json::Map<String, Value>> {
    let object = body.as_object().ok_or_else(|| {
        ApiError::BadRequest("Responses request body must be a JSON object".to_string())
    })?;
    if object
        .get("previous_response_id")
        .is_some_and(|value| !value.is_null())
        && object
            .get("conversation")
            .is_some_and(|value| !value.is_null())
    {
        return Err(ApiError::BadRequest(
            "previous_response_id and conversation cannot be used together".to_string(),
        ));
    }
    Ok(object)
}

fn validate_effective_responses_model(upstream_path: &str, model: &str) -> Result<()> {
    if upstream_path == "/responses/compact" && model.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "model is required for /v1/responses/compact when it cannot be resolved from previous_response_id"
                .to_string(),
        ));
    }
    Ok(())
}

/// Validate the known top-level Responses create fields that KeyCompute may
/// acknowledge without contacting an upstream. Unknown fields remain accepted
/// for forward compatibility and retain their complete JSON values.
///
/// WebSocket `generate:false` warmups do not execute the HTTP pipeline, so an
/// upstream cannot reject malformed known fields on KeyCompute's behalf.
pub(super) fn validate_responses_request(body: &Value) -> Result<()> {
    ResponsesRoutingFields::parse(body)?;
    let object = body
        .as_object()
        .expect("ResponsesRoutingFields validated the request as an object");

    for field in ["background", "parallel_tool_calls", "store", "stream"] {
        validate_optional_responses_field(object, field, "a boolean", Value::is_boolean)?;
    }
    for field in [
        "moderation",
        "prompt",
        "prompt_cache_options",
        "reasoning",
        "stream_options",
        "text",
    ] {
        validate_optional_responses_field(object, field, "an object", Value::is_object)?;
    }
    for field in [
        "instructions",
        "previous_response_id",
        "prompt_cache_key",
        "prompt_cache_retention",
        "service_tier",
        "truncation",
        "user",
    ] {
        validate_optional_responses_field(object, field, "a string", Value::is_string)?;
    }
    for field in ["context_management", "tools"] {
        validate_optional_responses_field(object, field, "an array of objects", |value| {
            value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_object))
        })?;
    }
    validate_optional_responses_field(object, "conversation", "a string or object", |value| {
        value.is_string()
            || value
                .as_object()
                .and_then(|conversation| conversation.get("id"))
                .is_some_and(Value::is_string)
    })?;
    validate_optional_responses_field(object, "include", "an array of strings", |value| {
        value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    })?;
    validate_optional_responses_field(object, "input", "a string or array of objects", |value| {
        value.is_string()
            || value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_object))
    })?;
    validate_optional_responses_field(object, "max_tool_calls", "an unsigned integer", |value| {
        value.as_u64().is_some()
    })?;
    validate_optional_responses_field(object, "metadata", "an object of string values", |value| {
        value.as_object().is_some_and(|metadata| {
            metadata.len() <= 16
                && metadata.iter().all(|(key, value)| {
                    key.chars().count() <= 64
                        && value
                            .as_str()
                            .is_some_and(|value| value.chars().count() <= 512)
                })
        })
    })?;
    validate_optional_responses_field(
        object,
        "safety_identifier",
        "a string of at most 64 characters",
        |value| {
            value
                .as_str()
                .is_some_and(|value| value.chars().count() <= 64)
        },
    )?;
    validate_optional_responses_field(object, "tool_choice", "a string or object", |value| {
        value.is_string() || value.is_object()
    })?;
    validate_optional_responses_field(
        object,
        "top_logprobs",
        "an integer between 0 and 20",
        |value| value.as_u64().is_some_and(|value| value <= 20),
    )?;
    Ok(())
}

fn validate_optional_responses_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    expected: &str,
    valid: impl FnOnce(&Value) -> bool,
) -> Result<()> {
    if let Some(value) = object.get(field)
        && !value.is_null()
        && !valid(value)
    {
        return Err(ApiError::BadRequest(format!(
            "{field} must be {expected} or null"
        )));
    }
    Ok(())
}

fn response_store_enabled(body: &Value) -> bool {
    body.get("store").and_then(Value::as_bool).unwrap_or(true)
}

fn response_affinity_storage_enabled(upstream_path: &str, body: &Value) -> bool {
    upstream_path == "/responses" && response_store_enabled(body)
}

pub(super) struct ResponsesWarmup<'a> {
    pub(super) request_id: uuid::Uuid,
    pub(super) response_id: &'a str,
    pub(super) request_body: Value,
    pub(super) response: Value,
    pub(super) context_items: &'a [Value],
    pub(super) request_state: &'a serde_json::Map<String, Value>,
    pub(super) upstream_previous_response_id: Option<&'a str>,
}

/// Persist a WebSocket `generate:false,store:true` warmup. Chained upstream
/// parents retain their owner; root warmups have no upstream account owner and
/// are routed normally for their first generation.
pub(super) async fn persist_responses_warmup(
    state: &AppState,
    auth: &AuthExtractor,
    warmup: ResponsesWarmup<'_>,
) -> Result<()> {
    let ResponsesWarmup {
        request_id,
        response_id,
        request_body,
        response,
        context_items,
        request_state,
        upstream_previous_response_id,
    } = warmup;
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden(
            "API-use permission is required for Responses warmups".to_string(),
        ));
    }
    let routing = ResponsesRoutingFields::parse(&request_body)?;
    let model = (!routing.model.is_empty()).then(|| routing.model.clone());
    let (provider, upstream_owner_account_id, mut reservations) = resolve_warmup_storage_owner(
        state,
        auth,
        request_id,
        &request_body,
        routing,
        upstream_previous_response_id,
    )
    .await?;
    // Owner resolution is the last consumer of the complete create body.
    // Release it before materializing the durable continuation JSON, which may
    // contain a second copy of large input or tool state.
    drop(request_body);
    let pool = state.pool.as_deref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Database is required for stored WebSocket warmups".to_string(),
        )
    })?;
    let expires_at = chrono::Utc::now()
        + chrono::Duration::from_std(RESPONSES_AFFINITY_TTL).unwrap_or(chrono::Duration::days(30));
    let local_context_bytes = estimated_json_bytes(&response)
        .saturating_add(estimated_context_bytes(context_items))
        .saturating_add(estimated_map_bytes(request_state))
        .saturating_add(
            upstream_previous_response_id
                .map(|id| id.len().saturating_add(64))
                .unwrap_or_default(),
        )
        .saturating_add(128);
    let local_context_bytes = i64::try_from(local_context_bytes).unwrap_or(i64::MAX);
    ResponseAffinity::upsert_local_with_quota(
        pool,
        auth.tenant_id,
        response_id,
        &provider,
        upstream_owner_account_id,
        response,
        json!({
            "items": context_items,
            "request_state": request_state,
            "upstream_previous_response_id": upstream_previous_response_id,
        }),
        local_context_bytes,
        expires_at,
        STORED_WARMUPS_PER_TENANT,
        STORED_WARMUP_BYTES_PER_TENANT,
    )
    .await
    .map_err(|error| map_response_affinity_write_error(error, "store Responses warmup"))?;
    if let Some(account_id) = upstream_owner_account_id {
        cache_response_affinity_best_effort(
            state,
            response_id,
            ResponsesAffinity {
                tenant_id: auth.tenant_id,
                provider,
                model,
                account_id,
                expires_at_unix: expires_at.timestamp(),
            },
        )
        .await;
    }
    reservations.release().await;
    Ok(())
}

async fn resolve_warmup_storage_owner(
    state: &AppState,
    auth: &AuthExtractor,
    request_id: uuid::Uuid,
    request_body: &Value,
    routing: ResponsesRoutingFields,
    upstream_previous_response_id: Option<&str>,
) -> Result<(String, Option<uuid::Uuid>, ResponsesExecutionReservations)> {
    if upstream_previous_response_id.is_some() {
        // A chained warmup must retain the upstream parent's account, and
        // the reservation prevents that account from changing while the
        // local continuation is persisted. Root warmups have no upstream
        // state and deliberately skip this entire routing path.
        let selected =
            select_responses_post_account(state, auth, request_id, request_body, routing, None)
                .await?;
        let (account, reservations) =
            reserve_selected_responses_account(state, auth.tenant_id, request_id, selected).await?;
        Ok((account.provider, Some(account.account_id), reservations))
    } else {
        Ok((
            "openai".to_string(),
            None,
            ResponsesExecutionReservations::default(),
        ))
    }
}

/// Load replay context for a stored local warmup. Upstream-created responses
/// have affinity rows too but no local context, so `None` means “let the
/// upstream resolve previous_response_id”.
pub(super) struct StoredWarmupContext {
    pub(super) items: Vec<Value>,
    pub(super) request_state: serde_json::Map<String, Value>,
    pub(super) upstream_previous_response_id: Option<String>,
    pub(super) model: Option<String>,
}

pub(super) async fn stored_warmup_context(
    state: &AppState,
    tenant_id: uuid::Uuid,
    response_id: &str,
) -> Result<Option<StoredWarmupContext>> {
    let Some(pool) = state.pool.as_deref() else {
        return Ok(None);
    };
    let Some(affinity) = ResponseAffinity::find_active_local_state(pool, tenant_id, response_id)
        .await
        .map_err(|error| ApiError::Internal(format!("Failed to load Responses warmup: {error}")))?
    else {
        return Ok(None);
    };
    let keycompute_db::models::response_affinity::LocalResponseState {
        model,
        local_context,
    } = affinity;
    match local_context {
        Value::Object(mut context) => Ok(Some(StoredWarmupContext {
            items: context
                .remove("items")
                .and_then(|items| match items {
                    Value::Array(items) => Some(items),
                    _ => None,
                })
                .unwrap_or_default(),
            request_state: context
                .remove("request_state")
                .and_then(|state| match state {
                    Value::Object(state) => Some(state),
                    _ => None,
                })
                .unwrap_or_default(),
            upstream_previous_response_id: context
                .remove("upstream_previous_response_id")
                .and_then(|value| value.as_str().map(str::to_string)),
            model,
        })),
        _ => Ok(None),
    }
}

pub(super) async fn stored_warmup_context_size(
    state: &AppState,
    tenant_id: uuid::Uuid,
    response_id: &str,
) -> Result<Option<usize>> {
    let Some(pool) = state.pool.as_deref() else {
        return Ok(None);
    };
    ResponseAffinity::find_active_local_context_size(pool, tenant_id, response_id)
        .await
        .map(|size| size.map(|bytes| usize::try_from(bytes).unwrap_or(usize::MAX)))
        .map_err(|error| {
            ApiError::Internal(format!("Failed to inspect Responses warmup size: {error}"))
        })
}

fn admit_stored_warmup_http_context(
    state: &AppState,
    resident_bytes: usize,
    body_permit: &mut Option<ResponsesHttpBodyPermit>,
) -> Result<()> {
    if resident_bytes > OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES {
        return Err(ApiError::BadRequest(
            "The Responses request and its stored continuation context exceed this server's memory limit."
                .to_string(),
        ));
    }
    if body_permit.is_some() || resident_bytes as u64 <= RESPONSES_LARGE_HTTP_BODY_BYTES {
        return Ok(());
    }
    *body_permit = Some(
        state
            .responses_http_body_admission
            .try_acquire()
            .ok_or_else(|| {
                ApiError::RateLimit("Too many large Responses payloads are active".to_string())
            })?,
    );
    Ok(())
}

/// Expand a persisted WebSocket warmup before an HTTP create/count request is
/// sent upstream. The synthetic `resp_ws_*` ID exists only at KeyCompute; the
/// owning upstream instead receives the accumulated input and, when present,
/// the last real stored upstream response ID.
async fn replay_stored_warmup_body(
    state: &AppState,
    tenant_id: uuid::Uuid,
    body: &mut Value,
    body_permit: &mut Option<ResponsesHttpBodyPermit>,
) -> Result<Option<String>> {
    let Some(client_previous_response_id) = body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(None);
    };
    if let Some(context_bytes) =
        stored_warmup_context_size(state, tenant_id, &client_previous_response_id).await?
    {
        let resident_bytes = estimated_json_bytes(body).saturating_add(context_bytes);
        admit_stored_warmup_http_context(state, resident_bytes, body_permit)?;
    }
    let Some(context) =
        stored_warmup_context(state, tenant_id, &client_previous_response_id).await?
    else {
        return Ok(None);
    };

    apply_stored_warmup_context(body, context)?;
    Ok(Some(client_previous_response_id))
}

fn apply_stored_warmup_context(body: &mut Value, context: StoredWarmupContext) -> Result<()> {
    let object = body.as_object_mut().ok_or_else(|| {
        ApiError::BadRequest("Responses request body must be a JSON object".to_string())
    })?;
    let current_input = object.remove("input");
    let StoredWarmupContext {
        mut items,
        request_state,
        upstream_previous_response_id,
        model,
    } = context;
    items.extend(normalize_owned_responses_input(current_input));
    apply_stored_warmup_request_state(body, request_state);
    let object = body
        .as_object_mut()
        .expect("the Responses body was validated as an object above");
    if object.get("model").is_none_or(Value::is_null)
        && let Some(model) = model
    {
        object.insert("model".to_string(), Value::String(model));
    }
    object.insert("input".to_string(), Value::Array(items));
    if let Some(upstream_previous_response_id) = upstream_previous_response_id {
        // The official request contract makes conversation and
        // previous_response_id mutually exclusive. A chained local warmup may
        // inherit conversation state from its root, but the real upstream
        // response ID is the more precise continuation once it exists.
        object.remove("conversation");
        object.insert(
            "previous_response_id".to_string(),
            Value::String(upstream_previous_response_id),
        );
    } else {
        object.remove("previous_response_id");
    }
    Ok(())
}

fn normalize_owned_responses_input(input: Option<Value>) -> Vec<Value> {
    match input {
        Some(Value::Array(items)) => items,
        Some(Value::String(text)) => vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        })],
        Some(Value::Null) | None => Vec::new(),
        Some(value) => vec![value],
    }
}

pub(super) fn estimated_context_bytes(items: &[Value]) -> usize {
    items.iter().fold(24, |size, item| {
        size.saturating_add(estimated_json_bytes(item))
    })
}

pub(super) fn estimated_map_bytes(map: &serde_json::Map<String, Value>) -> usize {
    map.iter().fold(32, |size, (key, value)| {
        size.saturating_add(64)
            .saturating_add(key.len())
            .saturating_add(estimated_json_bytes(value))
    })
}

pub(super) fn estimated_json_bytes(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => 32,
        Value::String(value) => 32usize.saturating_add(value.len()),
        Value::Array(values) => values.iter().fold(32, |size, value| {
            size.saturating_add(estimated_json_bytes(value))
        }),
        Value::Object(values) => estimated_map_bytes(values),
    }
}

fn apply_stored_warmup_request_state(
    body: &mut Value,
    request_state: serde_json::Map<String, Value>,
) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    for (name, value) in request_state {
        object.entry(name).or_insert(value);
    }
}

fn optional_u32(value: Option<&Value>, field: &str) -> Result<Option<u32>> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let value = value.as_u64().ok_or_else(|| {
        ApiError::BadRequest(format!("{field} must be an unsigned integer or null"))
    })?;
    u32::try_from(value)
        .map(Some)
        .map_err(|_| ApiError::BadRequest(format!("{field} exceeds the supported range")))
}

fn optional_f32(value: Option<&Value>, field: &str) -> Result<Option<f32>> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| ApiError::BadRequest(format!("{field} must be a finite number or null")))?;
    Ok(Some(value as f32))
}

/// POST /v1/responses/input_tokens
pub async fn count_response_input_tokens(
    State(state): State<AppState>,
    auth: AuthExtractor,
    request_id: RequestId,
    headers: HeaderMap,
    (body_permit, Json(mut body)): (Option<Extension<ResponsesHttpBodyPermit>>, Json<Value>),
) -> Result<Response> {
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden(
            "API-use permission is required for /v1/responses/input_tokens".to_string(),
        ));
    }
    let forwarded_headers = forwarded_responses_headers(&headers, auth.tenant_id)?;
    // Preserve the official mutual-exclusion check on the client request even
    // though replay may replace or remove its synthetic previous-response ID.
    validate_responses_reference_fields(&body)?;
    // A persisted WebSocket warmup is a KeyCompute-local resource. Expand it
    // before routing so the synthetic resp_ws_* affinity cannot pin token
    // counting to the placeholder account used only to satisfy durable storage.
    // The effective model, conversation, and real upstream previous-response ID
    // must drive account selection.
    let mut body_permit = body_permit.map(|Extension(permit)| permit);
    replay_stored_warmup_body(&state, auth.tenant_id, &mut body, &mut body_permit).await?;
    let routing = ResponsesRoutingFields::parse_input_tokens(&body)?;
    let conversation_id = conversation_resource_id(&body).map(str::to_string);
    let selected = select_responses_post_account(
        &state,
        &auth,
        request_id.0,
        &body,
        routing,
        headers
            .get("openai-beta")
            .and_then(|value| value.to_str().ok()),
    )
    .await?;
    let (account, reservations) =
        reserve_selected_responses_account(&state, auth.tenant_id, request_id.0, selected).await?;
    let client = state
        .http_proxy
        .client_for_provider_and_account(&account.provider, Some(account.account_id));
    let url = format!(
        "{}/responses/input_tokens",
        account.endpoint.trim_end_matches('/')
    );
    let mut request_headers = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", account.api_key),
        ),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    for (name, value) in forwarded_headers {
        request_headers.push((name, value));
    }
    let body = serde_json::to_string(&body).map_err(|error| {
        ApiError::Internal(format!("Failed to serialize input token request: {error}"))
    })?;
    let response = client
        .request_json_passthrough(
            JsonRequestMethod::Post,
            &url,
            request_headers,
            Some(body),
            false,
        )
        .await
        .map_err(crate::error::map_execution_error)?;
    if (200..300).contains(&response.meta.status)
        && let Some(conversation_id) = conversation_id
    {
        save_response_affinity(
            &state,
            &conversation_id,
            ResponsesResourceKind::Conversation,
            ResponsesAffinityRoute {
                tenant_id: auth.tenant_id,
                provider: account.provider.clone(),
                model: account.model.clone(),
                account_id: account.account_id,
            },
            None,
        )
        .await?;
    }
    passthrough_response(response, Some(reservations), false)
}

async fn select_responses_post_account(
    state: &AppState,
    auth: &AuthExtractor,
    request_id: uuid::Uuid,
    body: &Value,
    routing: ResponsesRoutingFields,
    openai_beta: Option<&str>,
) -> Result<SelectedResponsesAccount> {
    if let Some(previous_response_id) = body.get("previous_response_id").and_then(Value::as_str) {
        return resolve_response_account(state, previous_response_id, auth.tenant_id)
            .await
            .map(|account| SelectedResponsesAccount {
                account,
                constraint: Some(ResponsesReservationConstraint::Affinity {
                    resource_id: previous_response_id.to_string(),
                }),
            });
    }
    let conversation_id = conversation_resource_id(body);
    if let Some(conversation_id) = conversation_id {
        match resolve_response_account(state, conversation_id, auth.tenant_id).await {
            Ok(account) => {
                return Ok(SelectedResponsesAccount {
                    account,
                    constraint: Some(ResponsesReservationConstraint::Affinity {
                        resource_id: conversation_id.to_string(),
                    }),
                });
            }
            Err(ApiError::NotFound(_)) => {
                let account = discover_conversation_account(
                    state,
                    conversation_id,
                    auth.tenant_id,
                    openai_beta,
                )
                .await?;
                let constraint = ResponsesReservationConstraint::discovered(&account);
                return Ok(SelectedResponsesAccount {
                    account,
                    constraint: Some(constraint),
                });
            }
            Err(error) => return Err(error),
        }
    }
    let provider = keycompute_pricing::resolve_pricing_provider(&routing.model);
    let pricing = state
        .pricing
        .create_snapshot(&routing.model, &auth.tenant_id, Some(provider))
        .await
        .map_err(|error| {
            ApiError::Internal(format!("Failed to create pricing snapshot: {error}"))
        })?;
    let mut ctx = RequestContext::new(
        request_id,
        auth.user_id,
        auth.tenant_id,
        auth.produce_ai_key_id,
        routing.model,
        routing.messages,
        false,
        pricing,
    );
    // input_tokens is a Responses resource even though it bypasses the normal
    // generation adapter. Mark the request before routing so a chat-only account
    // can never be selected for the direct passthrough call below.
    // Routing only needs the protocol marker here; the direct passthrough below
    // still owns the original body. Do not duplicate a potentially 80 MiB
    // inline-skills request merely to select a Responses-capable account.
    ctx.native_openai_responses_request = Some(Arc::new(json!({})));
    let ctx = Arc::new(ctx);
    let plan = state
        .routing
        .route(&ctx)
        .await
        .map_err(|error| crate::error::map_routing_error(error, "openai responses"))?;
    validate_project_resource_route(body, &plan)?;
    match plan.primary {
        ExecutionTarget::ProviderAccount {
            provider,
            account_id,
            endpoint,
            upstream_api_key,
        } if provider.eq_ignore_ascii_case("openai") => Ok(SelectedResponsesAccount {
            account: ResolvedResponsesAccount {
                provider,
                model: (!ctx.model.is_empty()).then(|| ctx.model.clone()),
                account_id,
                endpoint,
                api_key: upstream_api_key.expose().to_string(),
            },
            constraint: None,
        }),
        ExecutionTarget::ProviderAccount { .. } => Err(ApiError::BadRequest(
            "The model is not available through an OpenAI-compatible provider".to_string(),
        )),
        ExecutionTarget::Node { .. } => Err(ApiError::BadRequest(
            "Responses input token counting cannot be routed to a node".to_string(),
        )),
    }
}

async fn reserve_selected_responses_account(
    state: &AppState,
    tenant_id: uuid::Uuid,
    request_id: uuid::Uuid,
    selected: SelectedResponsesAccount,
) -> Result<(ResolvedResponsesAccount, ResponsesExecutionReservations)> {
    let model = selected.account.model.clone();
    let mut plan = ExecutionPlan::new(selected.account.into_target());
    let reservations = ResponsesExecutionReservations::acquire(
        state,
        tenant_id,
        request_id,
        &mut plan,
        selected.constraint.as_ref(),
    )
    .await?;
    let ExecutionTarget::ProviderAccount {
        provider,
        account_id,
        endpoint,
        upstream_api_key,
    } = plan.primary
    else {
        return Err(ApiError::Internal(
            "A reserved Responses account became a node route".to_string(),
        ));
    };
    Ok((
        ResolvedResponsesAccount {
            provider,
            model,
            account_id,
            endpoint,
            api_key: upstream_api_key.expose().to_string(),
        },
        reservations,
    ))
}

/// GET /v1/responses/{response_id}
pub async fn retrieve_response(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Path(response_id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden(
            "API-use permission is required for Responses resources".to_string(),
        ));
    }
    if let Some(response) = local_response_affinity(&state, auth.tenant_id, &response_id).await? {
        if response_query_requests_stream(query.as_deref()) {
            return local_response_stream(response, query.as_deref());
        }
        return Ok(Json(response).into_response());
    }
    proxy_response_resource(
        &state,
        &auth,
        &response_id,
        ResponseResourceOperation::Retrieve,
        query.as_deref(),
        &headers,
    )
    .await
}

/// DELETE /v1/responses/{response_id}
pub async fn delete_response(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Path(response_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden(
            "API-use permission is required for Responses resources".to_string(),
        ));
    }
    if let Some(pool) = state.pool.as_deref()
        && ResponseAffinity::has_pending_settlement(pool, auth.tenant_id, &response_id)
            .await
            .map_err(|error| {
                ApiError::Internal(format!(
                    "Failed to check Responses billing settlement: {error}"
                ))
            })?
    {
        return Err(ApiError::Conflict(
            "This background response cannot be deleted until billing settlement completes"
                .to_string(),
        ));
    }
    if local_response_affinity(&state, auth.tenant_id, &response_id)
        .await?
        .is_some()
    {
        delete_response_affinity(&state, &response_id, auth.tenant_id).await?;
        return Ok(deleted_response(&response_id));
    }
    proxy_response_resource(
        &state,
        &auth,
        &response_id,
        ResponseResourceOperation::Delete,
        None,
        &headers,
    )
    .await
}

/// POST /v1/responses/{response_id}/cancel
pub async fn cancel_response(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Path(response_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden(
            "API-use permission is required for Responses resources".to_string(),
        ));
    }
    if local_response_affinity(&state, auth.tenant_id, &response_id)
        .await?
        .is_some()
    {
        return Err(ApiError::BadRequest(
            "Cannot cancel a completed response".to_string(),
        ));
    }
    proxy_response_resource(
        &state,
        &auth,
        &response_id,
        ResponseResourceOperation::Cancel,
        None,
        &headers,
    )
    .await
}

/// GET /v1/responses/{response_id}/input_items
pub async fn list_response_input_items(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Path(response_id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden(
            "API-use permission is required for Responses resources".to_string(),
        ));
    }
    if let Some(pool) = state.pool.as_deref()
        && let Some(context_bytes) =
            ResponseAffinity::find_active_local_context_size(pool, auth.tenant_id, &response_id)
                .await
                .map_err(|error| {
                    ApiError::Internal(format!("Failed to inspect Responses input items: {error}"))
                })?
    {
        let mut context_permit = None;
        admit_stored_warmup_http_context(
            &state,
            usize::try_from(context_bytes).unwrap_or(usize::MAX),
            &mut context_permit,
        )?;
        if let Some(affinity) =
            ResponseAffinity::find_active_local_state(pool, auth.tenant_id, &response_id)
                .await
                .map_err(|error| {
                    ApiError::Internal(format!("Failed to load Responses input items: {error}"))
                })?
        {
            let data = match affinity.local_context {
                Value::Object(mut context) => context
                    .remove("items")
                    .and_then(|items| match items {
                        Value::Array(items) => Some(items),
                        _ => None,
                    })
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            let mut response = Json(paginate_local_input_items(
                &response_id,
                data,
                query.as_deref(),
            )?)
            .into_response();
            if let Some(permit) = context_permit {
                retain_response_body_guard(&mut response, permit);
            }
            return Ok(response);
        }
    }
    proxy_response_resource(
        &state,
        &auth,
        &response_id,
        ResponseResourceOperation::ListInputItems,
        query.as_deref(),
        &headers,
    )
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputItemsOrder {
    Asc,
    Desc,
}

struct LocalInputItemsQuery {
    after: Option<String>,
    limit: usize,
    order: InputItemsOrder,
}

fn parse_local_input_items_query(query: Option<&str>) -> Result<LocalInputItemsQuery> {
    let mut parsed = LocalInputItemsQuery {
        after: None,
        limit: 20,
        order: InputItemsOrder::Desc,
    };
    let Some(query) = query else {
        return Ok(parsed);
    };
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match name.as_ref() {
            "after" => parsed.after = Some(value.into_owned()),
            "limit" => {
                let limit = value.parse::<usize>().map_err(|_| {
                    ApiError::BadRequest("limit must be an integer between 1 and 100".to_string())
                })?;
                if !(1..=100).contains(&limit) {
                    return Err(ApiError::BadRequest(
                        "limit must be between 1 and 100".to_string(),
                    ));
                }
                parsed.limit = limit;
            }
            "order" => {
                parsed.order = match value.as_ref() {
                    "asc" => InputItemsOrder::Asc,
                    "desc" => InputItemsOrder::Desc,
                    _ => {
                        return Err(ApiError::BadRequest(
                            "order must be 'asc' or 'desc'".to_string(),
                        ));
                    }
                };
            }
            // `include` affects optional nested fields. Local warmup items are
            // already retained losslessly, so there is nothing to hydrate.
            "include" | "include[]" => {}
            _ => {}
        }
    }
    Ok(parsed)
}

fn local_input_item_id(response_id: &str, index: usize, item: &Value) -> String {
    if let Some(id) = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        return id.to_string();
    }
    let mut digest = Sha256::new();
    digest.update(response_id.as_bytes());
    digest.update(b":");
    digest.update(index.to_string().as_bytes());
    format!("msg_kc_{}", hex::encode(&digest.finalize()[..16]))
}

fn paginate_local_input_items(
    response_id: &str,
    items: Vec<Value>,
    query: Option<&str>,
) -> Result<Value> {
    let query = parse_local_input_items_query(query)?;
    let mut items = items
        .into_iter()
        .enumerate()
        .map(|(index, mut item)| {
            let id = local_input_item_id(response_id, index, &item);
            if let Some(object) = item.as_object_mut() {
                object
                    .entry("id".to_string())
                    .or_insert_with(|| Value::String(id.clone()));
            }
            (id, item)
        })
        .collect::<Vec<_>>();
    if query.order == InputItemsOrder::Desc {
        items.reverse();
    }
    let start = if let Some(after) = query.after.as_deref() {
        items
            .iter()
            .position(|(id, _)| id == after)
            .map(|position| position + 1)
            .ok_or_else(|| ApiError::BadRequest("after is not a valid input item ID".to_string()))?
    } else {
        0
    };
    let has_more = items.len().saturating_sub(start) > query.limit;
    let page = items
        .into_iter()
        .skip(start)
        .take(query.limit)
        .collect::<Vec<_>>();
    let first_id = page.first().map(|(id, _)| id.clone());
    let last_id = page.last().map(|(id, _)| id.clone());
    let data = page.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
    Ok(json!({
        "object": "list",
        "data": data,
        "first_id": first_id,
        "last_id": last_id,
        "has_more": has_more,
    }))
}

async fn local_response_affinity(
    state: &AppState,
    tenant_id: uuid::Uuid,
    response_id: &str,
) -> Result<Option<Value>> {
    let Some(pool) = state.pool.as_deref() else {
        return Ok(None);
    };
    ResponseAffinity::find_active_local_response(pool, tenant_id, response_id)
        .await
        .map_err(|error| ApiError::Internal(format!("Failed to load local Response: {error}")))
}

#[derive(Clone)]
pub(super) struct ResolvedResponsesAccount {
    pub(super) provider: String,
    pub(super) model: Option<String>,
    pub(super) account_id: uuid::Uuid,
    pub(super) endpoint: String,
    pub(super) api_key: String,
}

#[derive(Clone)]
enum ResponsesReservationConstraint {
    Affinity {
        resource_id: String,
    },
    ConnectionSnapshot {
        account_id: uuid::Uuid,
        endpoint: String,
        api_key: String,
    },
}

impl ResponsesReservationConstraint {
    fn discovered(account: &ResolvedResponsesAccount) -> Self {
        Self::ConnectionSnapshot {
            account_id: account.account_id,
            endpoint: account.endpoint.clone(),
            api_key: account.api_key.clone(),
        }
    }
}

struct SelectedResponsesAccount {
    account: ResolvedResponsesAccount,
    constraint: Option<ResponsesReservationConstraint>,
}

impl ResolvedResponsesAccount {
    fn into_target(self) -> ExecutionTarget {
        ExecutionTarget::new_provider(self.provider, self.account_id, self.endpoint, self.api_key)
    }
}

async fn discover_conversation_account(
    state: &AppState,
    conversation_id: &str,
    tenant_id: uuid::Uuid,
    openai_beta: Option<&str>,
) -> Result<ResolvedResponsesAccount> {
    let pool = state.pool.as_deref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Database is required to resolve an unknown conversation".to_string(),
        )
    })?;
    // Fetch at most one row beyond the probe budget. This both bounds the
    // database result and lets the discovery loop distinguish a confirmed
    // miss from an eligible set that was intentionally truncated.
    let candidate_limit = RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES.saturating_add(1);
    let accounts = Account::find_tenant_discovery_candidates(
        pool,
        tenant_id,
        "openai",
        AccountApiCapability::Responses.as_str(),
        candidate_limit as u64,
    )
    .await
    .map_err(|error| {
        ApiError::Internal(format!(
            "Failed to load conversation discovery accounts: {error}"
        ))
    })?;
    discover_conversation_account_from_candidates(
        accounts,
        conversation_id,
        RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES,
        Duration::from_secs(state.gateway_config.request_timeout_secs)
            .min(RESPONSES_CONVERSATION_DISCOVERY_MAX_DURATION),
        |configured_account| async move {
            let protocol = ProtocolType::parse(&configured_account.provider).ok_or_else(|| {
                ApiError::Internal(format!(
                    "Conversation discovery account {} has an invalid protocol",
                    configured_account.id
                ))
            })?;
            let endpoint = if configured_account.endpoint.is_empty() {
                protocol.default_endpoint().to_string()
            } else {
                configured_account.endpoint.clone()
            };
            let upstream_api_key = super::admin_account::decrypt_account_api_key(
                &configured_account.upstream_api_key_encrypted,
            )?;
            let mut headers = vec![(
                "Authorization".to_string(),
                format!("Bearer {upstream_api_key}"),
            )];
            if let Some(openai_beta) = openai_beta {
                headers.push(("openai-beta".to_string(), openai_beta.to_string()));
            }
            let response = state
                .http_proxy
                .client_for_provider_and_account(
                    &configured_account.provider,
                    Some(configured_account.id),
                )
                .request_json_passthrough(
                    JsonRequestMethod::Get,
                    &upstream_resource_url(&endpoint, "conversations", conversation_id, ""),
                    headers,
                    None,
                    false,
                )
                .await
                .map_err(crate::error::map_execution_error)?;
            if (200..300).contains(&response.meta.status) {
                let PassthroughBody::Full(body) = response.body else {
                    return Err(ApiError::Provider(
                        "Upstream conversation lookup unexpectedly returned a stream".to_string(),
                    ));
                };
                let (body, mut admission) = body.into_parts();
                admit_responses_json_parse(&body, &mut admission)?;
                let body: Value = serde_json::from_str(&body).map_err(|error| {
                    ApiError::Provider(format!(
                        "Invalid upstream conversation lookup body: {error}"
                    ))
                })?;
                let _admission = admission;
                if !conversation_lookup_matches(&body, conversation_id) {
                    return Ok(None);
                }
                return Ok(Some(ResolvedResponsesAccount {
                    provider: configured_account.provider,
                    model: None,
                    account_id: configured_account.id,
                    endpoint,
                    api_key: upstream_api_key,
                }));
            }
            if response.meta.status == StatusCode::NOT_FOUND.as_u16() {
                return Ok(None);
            }
            Err(ApiError::Provider(format!(
                "Upstream conversation lookup failed with HTTP {}",
                response.meta.status
            )))
        },
    )
    .await
}

fn conversation_lookup_matches(body: &Value, expected_id: &str) -> bool {
    body.get("object").and_then(Value::as_str) == Some("conversation")
        && body.get("id").and_then(Value::as_str) == Some(expected_id)
}

async fn discover_conversation_account_from_candidates<I, F, Fut>(
    candidates: I,
    conversation_id: &str,
    max_candidates: usize,
    max_duration: Duration,
    mut probe: F,
) -> Result<ResolvedResponsesAccount>
where
    I: IntoIterator<Item = Account>,
    F: FnMut(Account) -> Fut,
    Fut: std::future::Future<Output = Result<Option<ResolvedResponsesAccount>>>,
{
    let discovery = async {
        let mut candidates = candidates.into_iter();
        let mut first_error = None;
        for configured_account in candidates.by_ref().take(max_candidates) {
            let account_id = configured_account.id;
            match probe(configured_account).await {
                Ok(Some(account)) => return Ok(account),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        %account_id,
                        error = %error,
                        "conversation discovery candidate failed"
                    );
                    first_error.get_or_insert(error);
                }
            }
        }
        if candidates.next().is_some() {
            return Err(ApiError::Conflict(format!(
                "Conversation owner could not be resolved within the {max_candidates}-account discovery limit"
            )));
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Err(ApiError::NotFound(format!(
            "Conversation not found: {conversation_id}"
        )))
    };

    tokio::time::timeout(max_duration, discovery)
        .await
        .map_err(|_| {
            ApiError::ServiceUnavailable(
                "Conversation account discovery timed out; please try again".to_string(),
            )
        })?
}

async fn resolve_response_account(
    state: &AppState,
    response_id: &str,
    tenant_id: uuid::Uuid,
) -> Result<ResolvedResponsesAccount> {
    let affinity = response_affinity(state, response_id, tenant_id).await?;
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Database not configured".to_string()))?;
    let account = Account::find_by_id_for_key_share(pool, affinity.account_id)
        .await
        .map_err(|error| ApiError::Internal(format!("Failed to load Responses account: {error}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Response not found: {response_id}")))?;
    if !responses_account_is_visible_to_tenant(&account, tenant_id) {
        return Err(ApiError::NotFound(format!(
            "Response not found: {response_id}"
        )));
    }
    if !account.provider.eq_ignore_ascii_case("openai")
        || !affinity.provider.eq_ignore_ascii_case("openai")
        || account.id != affinity.account_id
    {
        return Err(ApiError::Conflict(
            "The account owning this response is no longer OpenAI-compatible".to_string(),
        ));
    }
    // Capabilities control selection for new, unbound requests. This affinity
    // is the ownership record for an existing upstream resource, so an admin
    // removing the account from future Responses routing must not strand it.
    if !account.enabled {
        return Err(ApiError::ServiceUnavailable(
            "The account owning this response is disabled".to_string(),
        ));
    }
    let protocol = ProtocolType::parse(&account.provider).ok_or_else(|| {
        ApiError::Conflict("The account owning this response has an invalid protocol".to_string())
    })?;
    let endpoint = if account.endpoint.is_empty() {
        protocol.default_endpoint().to_string()
    } else {
        account.endpoint
    };
    Ok(ResolvedResponsesAccount {
        provider: account.provider,
        model: affinity.model,
        account_id: account.id,
        endpoint,
        api_key: super::admin_account::decrypt_account_api_key(
            &account.upstream_api_key_encrypted,
        )?,
    })
}

fn responses_account_is_visible_to_tenant(account: &Account, tenant_id: uuid::Uuid) -> bool {
    account.tenant_id == tenant_id || account.visibility == "global"
}

async fn proxy_response_resource(
    state: &AppState,
    auth: &AuthExtractor,
    response_id: &str,
    operation: ResponseResourceOperation,
    query: Option<&str>,
    client_headers: &HeaderMap,
) -> Result<Response> {
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden(
            "API-use permission is required for Responses resources".to_string(),
        ));
    }
    let forwarded_headers = forwarded_responses_headers(client_headers, auth.tenant_id)?;
    let account = resolve_response_account(state, response_id, auth.tenant_id).await?;
    let selected = SelectedResponsesAccount {
        account,
        constraint: Some(ResponsesReservationConstraint::Affinity {
            resource_id: response_id.to_string(),
        }),
    };
    let (account, mut reservations) =
        reserve_selected_responses_account(state, auth.tenant_id, uuid::Uuid::new_v4(), selected)
            .await?;
    let mut url = upstream_resource_url(
        &account.endpoint,
        "responses",
        response_id,
        operation.suffix(),
    );
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        url.push('?');
        url.push_str(query);
    }
    let client = state
        .http_proxy
        .client_for_provider_and_account(&account.provider, Some(account.account_id));
    let mut request_headers = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", account.api_key),
        ),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    request_headers.extend(forwarded_headers);
    let response = client
        .request_json_passthrough(
            operation.method(),
            &url,
            request_headers,
            None,
            response_query_requests_stream(query),
        )
        .await
        .map_err(crate::error::map_execution_error)?;
    if operation.removes_affinity_on_success() && delete_response_is_confirmed(response.meta.status)
    {
        delete_response_affinity(state, response_id, auth.tenant_id).await?;
        if response.meta.status == 404 {
            reservations.release().await;
            return Ok(deleted_response(response_id));
        }
    }
    let allow_empty_stream = matches!(operation, ResponseResourceOperation::Retrieve)
        && response_query_resumes_stream(query);
    passthrough_response(response, Some(reservations), allow_empty_stream)
}

fn deleted_response(response_id: &str) -> Response {
    Json(json!({
        "id": response_id,
        "object": "response",
        "deleted": true,
    }))
    .into_response()
}

fn delete_response_is_confirmed(status: u16) -> bool {
    (200..300).contains(&status) || status == 404
}

#[derive(Clone, Copy)]
enum ResponseResourceOperation {
    Retrieve,
    Delete,
    Cancel,
    ListInputItems,
}

impl ResponseResourceOperation {
    fn method(self) -> JsonRequestMethod {
        match self {
            Self::Retrieve | Self::ListInputItems => JsonRequestMethod::Get,
            Self::Delete => JsonRequestMethod::Delete,
            Self::Cancel => JsonRequestMethod::Post,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Retrieve | Self::Delete => "",
            Self::Cancel => "/cancel",
            Self::ListInputItems => "/input_items",
        }
    }

    fn removes_affinity_on_success(self) -> bool {
        matches!(self, Self::Delete)
    }
}

fn response_query_requests_stream(query: Option<&str>) -> bool {
    query.is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .any(|(name, value)| name == "stream" && value.eq_ignore_ascii_case("true"))
    })
}

fn response_query_resumes_stream(query: Option<&str>) -> bool {
    response_query_requests_stream(query)
        && query.is_some_and(|query| {
            url::form_urlencoded::parse(query.as_bytes()).any(|(name, _)| name == "starting_after")
        })
}

fn local_response_stream(response: Value, query: Option<&str>) -> Result<Response> {
    let starting_after = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .find_map(|(name, value)| (name == "starting_after").then_some(value))
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                ApiError::BadRequest("starting_after must be an unsigned integer".to_string())
            })
        })
        .transpose()?;
    let events = [
        (0_u64, "response.created", "in_progress"),
        (1, "response.in_progress", "in_progress"),
        (2, "response.completed", "completed"),
    ]
    .into_iter()
    .filter(move |(sequence_number, _, _)| {
        starting_after.is_none_or(|starting_after| *sequence_number > starting_after)
    })
    .map(move |(sequence_number, event_type, status)| {
        let mut response = response.clone();
        response["status"] = Value::String(status.to_string());
        if status != "completed" {
            response["completed_at"] = Value::Null;
        }
        Ok::<_, Infallible>(
            Event::default().event(event_type).data(
                json!({
                    "type": event_type,
                    "sequence_number": sequence_number,
                    "response": response,
                })
                .to_string(),
            ),
        )
    });
    Ok(Sse::new(futures::stream::iter(events)).into_response())
}

/// Keep an admission guard alive in the HTTP body and every emitted data chunk
/// rather than in response extensions. Hyper drops the response head early and
/// may retain a chunk after body EOF, so both owners cover slow clients.
struct GuardedResponseBytes<G> {
    bytes: bytes::Bytes,
    _guard: Arc<G>,
}

impl<G> AsRef<[u8]> for GuardedResponseBytes<G> {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

fn retain_response_body_guard<G>(response: &mut Response, guard: G)
where
    G: Send + Sync + 'static,
{
    let body = std::mem::take(response.body_mut()).into_data_stream();
    let guard = Arc::new(guard);
    let guarded = futures::stream::unfold((body, guard), |(mut body, guard)| async move {
        body.next().await.map(|item| {
            let item = item.map(|bytes| {
                bytes::Bytes::from_owner(GuardedResponseBytes {
                    bytes,
                    _guard: Arc::clone(&guard),
                })
            });
            (item, (body, guard))
        })
    });
    *response.body_mut() = Body::from_stream(guarded);
}

fn admit_responses_json_parse_with_limit(
    body: &str,
    max_working_set_bytes: usize,
    admission: &mut Option<LargeBodyPermit>,
) -> Result<()> {
    let working_set_bytes = estimated_json_parse_working_set_bytes(body.as_bytes());
    if working_set_bytes > max_working_set_bytes {
        return Err(ApiError::Provider(format!(
            "Upstream Responses JSON exceeds the {max_working_set_bytes}-byte working-set limit"
        )));
    }
    if admission.is_none() && working_set_bytes > LARGE_JSON_WORKING_SET_ADMISSION_BYTES {
        *admission = Some(try_acquire_large_body_permit().ok_or_else(|| {
            ApiError::ServiceUnavailable(RESPONSES_JSON_PROCESSING_CAPACITY_MESSAGE.to_string())
        })?);
    }
    Ok(())
}

fn admit_responses_json_parse(body: &str, admission: &mut Option<LargeBodyPermit>) -> Result<()> {
    admit_responses_json_parse_with_limit(body, MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES, admission)
}

fn passthrough_response(
    response: llm_protocol_provider::UpstreamResponse<PassthroughBody>,
    reservations: Option<ResponsesExecutionReservations>,
    allow_empty_stream: bool,
) -> Result<Response> {
    let status = StatusCode::from_u16(response.meta.status)
        .map_err(|_| ApiError::Internal("Upstream returned an invalid status".to_string()))?;
    let mut builder = Response::builder().status(status);
    for (name, value) in response.meta.headers {
        if !forwarded_upstream_response_header(&name) {
            continue;
        }
        let Ok(name) = HeaderName::try_from(name) else {
            continue;
        };
        let Ok(value) = HeaderValue::try_from(value) else {
            continue;
        };
        builder = builder.header(name, value);
    }
    let mut response_admission = None;
    let body = match response.body {
        PassthroughBody::Full(body) => {
            drop(reservations);
            let (mut body, mut admission) = body.into_parts();
            if status.is_success() {
                admit_responses_json_parse(&body, &mut admission)?;
                body = sanitize_successful_responses_body(body)?;
            }
            response_admission = admission;
            Body::from(body)
        }
        PassthroughBody::Stream(stream) => {
            let stream = sanitize_responses_passthrough_stream(stream, allow_empty_stream);
            let guarded = stream.map(move |item| {
                let _reservations = &reservations;
                item
            });
            Body::from_stream(guarded)
        }
    };
    let mut response = builder.body(body).map_err(|error| {
        ApiError::Internal(format!("Failed to build upstream response: {error}"))
    })?;
    if let Some(admission) = response_admission {
        retain_response_body_guard(&mut response, admission);
    }
    Ok(response)
}

fn sanitize_successful_responses_body(body: String) -> Result<String> {
    let Ok(mut parsed) = serde_json::from_str::<Value>(&body) else {
        return Ok(body);
    };
    if !sanitize_upstream_responses_error(None, &mut parsed) {
        return Ok(body);
    }
    // `Value` owns its strings. Release the potentially large wire body before
    // allocating the sanitized serialization so only two representations are
    // resident at once.
    drop(body);
    serde_json::to_string(&parsed).map_err(|error| {
        ApiError::Internal(format!(
            "Failed to encode sanitized Responses response: {error}"
        ))
    })
}

fn sanitize_responses_passthrough_stream(stream: ByteStream, allow_empty: bool) -> ByteStream {
    let events = if allow_empty {
        llm_protocol_openai::responses_stream::parse_responses_stream_allow_empty(stream)
    } else {
        llm_protocol_openai::responses_stream::parse_responses_stream(stream)
    };
    Box::pin(events.filter_map(|event| async move {
        match event {
            Ok(StreamEvent::Native {
                event:
                    NativeStreamEvent::OpenAiResponsesSse {
                        event,
                        mut data,
                        admission,
                    },
            }) => {
                let _ = sanitize_upstream_responses_error(Some(&event), &mut data);
                let encoded = format!("event: {event}\ndata: {data}\n\n");
                let bytes = if let Some(admission) = admission {
                    bytes::Bytes::from_owner(GuardedResponseBytes {
                        bytes: bytes::Bytes::from(encoded),
                        _guard: Arc::new(admission),
                    })
                } else {
                    bytes::Bytes::from(encoded)
                };
                Some(Ok(bytes))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        }
    }))
}

fn forwarded_upstream_response_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "content-type"
        || name == "x-request-id"
        || name == "request-id"
        || name == "openai-version"
        || name == "openai-processing-ms"
        || name == "retry-after"
        || name.starts_with("x-ratelimit-")
}

fn allowlisted_upstream_response_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .filter(|(name, _)| forwarded_upstream_response_header(name))
        .collect()
}

fn append_forwarded_upstream_response_headers(
    response: &mut Response,
    headers: Vec<(String, String)>,
) {
    for (name, value) in allowlisted_upstream_response_headers(headers) {
        let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value))
        else {
            continue;
        };
        response.headers_mut().insert(name, value);
    }
}

fn cacheable_responses_success_headers(ctx: &RequestContext) -> Vec<(String, String)> {
    let mut headers = allowlisted_upstream_response_headers(ctx.client_upstream_response_headers());
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("content-type".to_string(), "application/json".to_string()));
    }
    headers
}

/// POST /v1/responses
pub async fn responses(
    State(state): State<AppState>,
    auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received_at: RequestReceivedAt,
    headers: HeaderMap,
    (body_permit, Json(body)): (Option<Extension<ResponsesHttpBodyPermit>>, Json<Value>),
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
    (body_permit, Json(body)): (Option<Extension<ResponsesHttpBodyPermit>>, Json<Value>),
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
pub(super) async fn responses_inner(
    state: AppState,
    auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received_at: RequestReceivedAt,
    headers: HeaderMap,
    mut body: Value,
    mut body_permit: Option<ResponsesHttpBodyPermit>,
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
        super::PreExecutionTraceGuard::new(Arc::clone(&lifecycle), request_id.0);
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
            super::PreExecutionTraceGuard::new(Arc::clone(&lifecycle), request_id.0);
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
    if let Some(balance_service) = state.billing.balance_service()
        && let Err(error) = balance_service
            .check_balance_for_tenant(auth.user_id, auth.tenant_id)
            .await
    {
        pre_execution_guard
            .finish_failed(
                ErrorOrigin::Client,
                TraceErrorCategory::Balance,
                "insufficient_balance",
            )
            .await;
        return Err(ApiError::from(error));
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
    // routing projection.
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
                if let Err(error) = enforce_authenticated_tpm_limit(&state, &auth).await {
                    reservations.release().await;
                    abandon_unstarted_responses_idempotency_execution(&state, &execution).await;
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
                            "tpm_check_failed",
                        )
                    };
                    pre_execution_guard
                        .finish_failed(origin, category, code)
                        .await;
                    return Err(error);
                }
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
    if let Some(execution) = idempotency_execution.as_ref()
        && let Err(error) = mark_responses_idempotency_dispatched(&state, execution).await
    {
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
        super::ClientResponseGuard::new(Arc::clone(&lifecycle), Arc::clone(&ctx));
    pre_execution_guard.disarm();
    let mut rx = match tokio::time::timeout(
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
        let initial_event = rx.recv().await;
        if let Some(initial_failure) = initial_responses_stream_failure(initial_event.as_ref()) {
            reservations.release().await;
            persist_terminal_responses_outbox(
                &state,
                &ctx,
                &primary_provider,
                primary_account_id,
                None,
                false,
                Some(routing.model.clone()),
                "error",
            )
            .await;
            finalize_responses_billing_logged(
                &state,
                &billing,
                &ctx,
                &primary_provider,
                primary_account_id,
                "error",
            )
            .await;
            super::finish_client_response_trace(
                &lifecycle,
                &ctx,
                ClientResponseOutcome::ResponseFailed,
            )
            .await;
            client_response_guard.disarm();
            if let Some(upstream) = ctx.client_upstream_response() {
                return client_upstream_response(upstream);
            }
            return Err(initial_failure);
        }
        let success_headers = ctx.client_upstream_response_headers();
        let stream = create_responses_stream(
            rx,
            initial_event,
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
            },
        );
        client_response_guard.disarm();
        let mut response = Sse::new(stream).into_response();
        append_forwarded_upstream_response_headers(&mut response, success_headers);
        Ok(response)
    } else {
        client_response_guard.disarm();
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

fn initial_responses_stream_failure(
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

struct ResponsesJsonRuntime {
    ctx: Arc<RequestContext>,
    provider: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
    lifecycle: Arc<dyn RequestLifecycleRecorder>,
    state: AppState,
    client_previous_response_id: Option<String>,
    persist_response_affinity: bool,
    reservations: ResponsesExecutionReservations,
    body_permit: Option<ResponsesHttpBodyPermit>,
    idempotency_execution: Option<ResponsesIdempotencyExecution>,
}

async fn create_responses_json(
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
    let mut guard = super::ClientResponseGuard::new(Arc::clone(&lifecycle), Arc::clone(&ctx));
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
            super::finish_client_response_trace(
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
            super::finish_client_response_trace(
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
    if let Err(error) = super::record_final_client_first_content(&lifecycle, ctx.request_id).await {
        tracing::warn!(request_id = %ctx.request_id, %error, "failed to record client first content");
    }
    super::finish_client_response_trace(&lifecycle, &ctx, response_client_outcome(&response)).await;
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

fn cacheable_upstream_responses_error(
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

const BACKGROUND_SETTLEMENT_MAX: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackgroundSettlement {
    request_id: uuid::Uuid,
    #[serde(default)]
    billing_request_id: Option<uuid::Uuid>,
    tenant_id: uuid::Uuid,
    user_id: uuid::Uuid,
    produce_ai_key_id: uuid::Uuid,
    model: String,
    provider: String,
    account_id: uuid::Uuid,
    pricing_snapshot: keycompute_types::PricingSnapshot,
    started_at: chrono::DateTime<chrono::Utc>,
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    input_tokens_finalized: bool,
    #[serde(default)]
    output_tokens_finalized: bool,
    #[serde(default)]
    openai_beta: Option<String>,
    #[serde(default)]
    terminal_status: Option<String>,
    // A terminal status without a terminal timestamp is the durable marker for
    // a pending response that exhausted its settlement deadline. Its usage is
    // still billed, but must not be shifted into the current TPM window.
    #[serde(default)]
    terminal_at: Option<chrono::DateTime<chrono::Utc>>,
    deadline_at: chrono::DateTime<chrono::Utc>,
    attempt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponsesTpmTiming {
    LedgerFinishedAt,
    TerminalAt(chrono::DateTime<chrono::Utc>),
    Skip,
}

fn background_settlement_tpm_timing(
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

enum BackgroundPollOutcome {
    Response {
        body: Value,
        billing_status: Option<&'static str>,
        admission: Option<LargeBodyPermit>,
    },
    Retry,
    TerminalHttpError(u16),
}

fn background_settlement_value(
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

fn terminal_settlement_value(
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

fn terminal_settlement_value_with_tpm_timing(
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

fn settlement_billing_request_id(settlement: &BackgroundSettlement) -> uuid::Uuid {
    settlement
        .billing_request_id
        .unwrap_or(settlement.request_id)
}

fn settlement_affinity_account_id(settlement: &BackgroundSettlement) -> Option<uuid::Uuid> {
    (!settlement.account_id.is_nil()).then_some(settlement.account_id)
}

fn background_billing_context(
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

fn spawn_background_settlement(
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

fn background_account_snapshot(
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

fn background_settlement_context(ctx: &RequestContext) -> Arc<RequestContext> {
    Arc::new(ctx.clone_without_request_payloads())
}

/// Start replica-safe Responses maintenance. Durable settlement jobs are
/// leased with `SKIP LOCKED`; expired affinity rows are removed periodically.
pub fn spawn_responses_maintenance(state: AppState) {
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

fn take_available_settlement_permits(
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

async fn settle_background_job(state: &AppState, affinity: ResponseAffinity) {
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

fn authoritative_responses_token_counts(
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

async fn background_poll_account(
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
    let api_key =
        super::admin_account::decrypt_account_api_key(&account.upstream_api_key_encrypted)?;
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

fn background_poll_status_is_retryable(status: u16) -> bool {
    matches!(status, 404 | 408 | 409 | 429) || status >= 500
}

fn background_poll_parse_error_is_retryable(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::ServiceUnavailable(message)
            if message == RESPONSES_JSON_PROCESSING_CAPACITY_MESSAGE
    )
}

fn background_response_terminal_at(body: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let completed_at = body.get("completed_at")?.as_i64()?;
    chrono::DateTime::from_timestamp(completed_at, 0)
}

/// Validate a retrieved response before its usage can affect an immutable
/// billing settlement. A configured compatibility endpoint is still an
/// untrusted protocol peer: a successful HTTP status must not let it attach a
/// different response's usage to this job or turn arbitrary JSON into a
/// terminal result.
fn validate_background_response(
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

async fn clear_claimed_background_job(pool: &keycompute_db::DbRouter, affinity: &ResponseAffinity) {
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
async fn settle_background_response(
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

fn response_is_background_pending(body: &Value) -> bool {
    matches!(
        body.get("status").and_then(Value::as_str),
        Some("queued" | "in_progress")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResponseUsageUpdate {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    output_is_estimate: bool,
}

fn response_usage_update(body: &Value) -> ResponseUsageUpdate {
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

fn apply_response_usage(ctx: &RequestContext, body: &Value) {
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

fn apply_response_event_usage(ctx: &RequestContext, body: &Value) {
    apply_response_usage(ctx, body.get("response").unwrap_or(body));
}

const SSE_SEND_TIMEOUT: Duration = Duration::from_secs(30);

struct ResponsesStreamRuntime {
    ctx: Arc<RequestContext>,
    provider: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
    lifecycle: Arc<dyn RequestLifecycleRecorder>,
    state: AppState,
    client_previous_response_id: Option<String>,
    persist_response_affinity: bool,
    reservations: ResponsesExecutionReservations,
    body_permit: Option<ResponsesHttpBodyPermit>,
}

/// Secure foreground stream settlement at most once. A scheduled background
/// job already owns settlement, so both that state and a successful foreground
/// attempt satisfy later terminal events without invoking `settle` again.
async fn secure_responses_stream_settlement_once<F, Fut>(
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
async fn secure_responses_stream_settlement(
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

fn create_responses_stream(
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
                                    super::finish_client_response_trace(
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
                                        super::finish_client_response_trace(
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
                                super::finish_client_response_trace(
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
                            super::finish_client_response_trace(&lifecycle, &ctx, outcome).await;
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
                                super::finish_client_response_trace(
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
                            super::finish_client_response_trace(
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
            super::finish_client_response_trace(
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

struct AdmittedSseEvent {
    event: Event,
    admission: Option<LargeBodyPermit>,
}

async fn forward_sse_event(
    tx: &mpsc::Sender<AdmittedSseEvent>,
    ctx: &RequestContext,
    connected: &mut bool,
    event: Event,
) -> bool {
    forward_admitted_sse_event(tx, ctx, connected, event, None).await
}

async fn forward_admitted_sse_event(
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

fn responses_error_event(message: &str) -> Event {
    Event::default().event("error").data(
        json!({
            "type": "error",
            "code": "server_error",
            "message": message,
            "param": null,
            "sequence_number": 0,
        })
        .to_string(),
    )
}

/// Preserve the stable fields SDKs use for classification while applying the
/// same free-form-message policy as non-streaming Responses HTTP errors.
fn sanitize_upstream_responses_error(event: Option<&str>, body: &mut Value) -> bool {
    let body_type = body.get("type").and_then(Value::as_str);
    if event == Some("error") || body_type == Some("error") {
        let code = sanitize_openai_responses_error_code(
            body.get("code"),
            Value::String("server_error".to_string()),
        );
        let param = sanitize_openai_responses_error_param(body.get("param"));
        let sequence_number = body
            .get("sequence_number")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        *body = json!({
            "type": "error",
            "code": code,
            "message": "Upstream request failed",
            "param": param,
            "sequence_number": sequence_number,
        });
        return true;
    }

    if event == Some("response.failed") || body_type == Some("response.failed") {
        if let Some(error) = body.pointer_mut("/response/error") {
            sanitize_upstream_response_error_object(error);
            return true;
        }
        return false;
    }

    if body.get("object").and_then(Value::as_str) == Some("response")
        && body.get("status").and_then(Value::as_str) == Some("failed")
        && let Some(error) = body.get_mut("error")
    {
        sanitize_upstream_response_error_object(error);
        return true;
    }
    false
}

fn sanitize_upstream_response_error_object(error: &mut Value) {
    let code = sanitize_openai_responses_error_code(
        error.get("code"),
        Value::String("server_error".to_string()),
    );
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .map(|_| sanitize_openai_responses_error_type(error.get("type"), "server_error"));
    let param = error
        .get("param")
        .map(|_| sanitize_openai_responses_error_param(error.get("param")));
    let mut sanitized = serde_json::Map::new();
    sanitized.insert("code".to_string(), code);
    sanitized.insert(
        "message".to_string(),
        Value::String("Upstream request failed".to_string()),
    );
    if let Some(error_type) = error_type {
        sanitized.insert("type".to_string(), error_type);
    }
    if let Some(param) = param {
        sanitized.insert("param".to_string(), param);
    }
    *error = Value::Object(sanitized);
}

fn response_resource_id(body: &Value) -> Option<&str> {
    body.get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_response_id(id))
}

fn response_event_resource_id(body: &Value) -> Option<&str> {
    body.pointer("/response/id")
        .and_then(Value::as_str)
        .filter(|id| valid_response_id(id))
}

fn conversation_resource_id(body: &Value) -> Option<&str> {
    let conversation = body
        .pointer("/response/conversation")
        .or_else(|| body.get("conversation"))?;
    conversation
        .as_str()
        .or_else(|| conversation.get("id").and_then(Value::as_str))
        .filter(|id| valid_conversation_id(id))
}

fn validate_idempotent_project_resource_owner(
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
fn validate_project_resource_route(body: &Value, plan: &ExecutionPlan) -> Result<()> {
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
fn project_scoped_responses_resource(body: &Value) -> Option<&'static str> {
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

fn project_scoped_input_resource(value: &Value) -> Option<&'static str> {
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

fn project_scoped_tool_resource(tool: &Value) -> Option<&'static str> {
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

fn non_empty_string(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

fn contains_non_empty_string(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|values| values.iter().any(|value| non_empty_string(value).is_some()))
}

fn patch_client_previous_response_id(body: &mut Value, previous_response_id: Option<&str>) {
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

fn local_warmup_has_no_upstream_owner(
    replayed_client_previous_response_id: Option<&str>,
    effective_body: &Value,
) -> bool {
    replayed_client_previous_response_id.is_some()
        && effective_body
            .get("previous_response_id")
            .is_none_or(Value::is_null)
        && conversation_resource_id(effective_body).is_none()
}

fn valid_response_id(id: &str) -> bool {
    llm_protocol_openai::responses_stream::valid_openai_resource_id(id)
}

fn valid_conversation_id(id: &str) -> bool {
    llm_protocol_openai::responses_stream::valid_openai_resource_id(id)
}

fn valid_affinity_resource_id(id: &str) -> bool {
    valid_response_id(id) || valid_conversation_id(id)
}

fn upstream_resource_url(
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

const RESPONSES_AFFINITY_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const RESPONSES_AFFINITY_LOCAL_MAX: usize = 100_000;
const RESPONSES_EXECUTION_RESERVATION_MIN_TTL: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponsesResourceKind {
    Response,
    Conversation,
}

struct ResponsesAffinityRoute {
    tenant_id: uuid::Uuid,
    provider: String,
    model: Option<String>,
    account_id: uuid::Uuid,
}

fn responses_execution_reservation_ttl(config: &keycompute_config::GatewayConfig) -> Duration {
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
trait ResponsesReservationCleanup: Send + Sync {
    async fn delete(
        &self,
        tenant_id: uuid::Uuid,
        reservation_id: &str,
    ) -> std::result::Result<(), String>;
}

struct DatabaseResponsesReservationCleanup {
    pool: Arc<keycompute_db::DbRouter>,
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
struct ResponsesExecutionReservations {
    tenant_id: uuid::Uuid,
    reservation_ids: Vec<String>,
    cleanup: Option<Arc<dyn ResponsesReservationCleanup>>,
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
    async fn acquire(
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

    fn is_empty(&self) -> bool {
        self.reservation_ids.is_empty()
    }

    async fn release(&mut self) {
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
    fn retain_until_expiry(&mut self) {
        self.reservation_ids.clear();
    }
}

async fn reserve_responses_execution_target(
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

async fn validate_responses_reservation_constraint(
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

fn reserved_responses_account_snapshot(
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
        api_key: super::admin_account::decrypt_account_api_key(
            &account.upstream_api_key_encrypted,
        )?,
    })
}

fn reserved_responses_account_is_available(
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

fn apply_reserved_account_snapshots(
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

fn validate_reserved_idempotency_connection(
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

fn affinity_storage_key(tenant_id: uuid::Uuid, response_id: &str) -> String {
    format!("{tenant_id}:{response_id}")
}

fn affinity_cache_key(tenant_id: uuid::Uuid, response_id: &str) -> String {
    format!(
        "responses:affinity:{}",
        affinity_storage_key(tenant_id, response_id)
    )
}

const RESPONSES_STATE_UNAVAILABLE_MESSAGE: &str = "Responses state is temporarily unavailable";

fn responses_state_unavailable(operation: &str, error: impl std::fmt::Display) -> ApiError {
    tracing::error!(%error, operation, "Responses state operation failed");
    ApiError::ServiceUnavailable(RESPONSES_STATE_UNAVAILABLE_MESSAGE.to_string())
}

fn map_response_affinity_write_error(error: keycompute_db::DbError, operation: &str) -> ApiError {
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

fn map_responses_idempotency_bind_error(error: keycompute_db::DbError) -> ApiError {
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

async fn cache_response_affinity_best_effort(
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

async fn save_response_affinity(
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

async fn save_response_affinity_if_stored(
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
async fn persist_terminal_responses_outbox(
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
async fn persist_terminal_responses_outbox_with_tpm_timing(
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
pub(super) async fn persist_immediate_terminal_settlement_outbox(
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

pub(super) async fn acknowledge_terminal_settlement_outbox(state: &AppState, ctx: &RequestContext) {
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

fn terminal_settlement_outbox_targets(
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

fn settlement_next_poll_at(settlement: &Value) -> chrono::DateTime<chrono::Utc> {
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

fn affinity_expiry_unix(resource_kind: ResponsesResourceKind, now: i64) -> i64 {
    if resource_kind == ResponsesResourceKind::Conversation {
        // Conversations remain usable until explicitly deleted upstream. Keep
        // their durable tenant/account binding rather than applying the
        // shorter stored-Response retention window.
        chrono::DateTime::<chrono::Utc>::MAX_UTC.timestamp()
    } else {
        now.saturating_add(i64::try_from(RESPONSES_AFFINITY_TTL.as_secs()).unwrap_or(i64::MAX))
    }
}

async fn response_affinity(
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

async fn cache_response_affinity_locally(
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

fn insert_response_affinity_with_limit(
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

async fn delete_response_affinity(
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

fn is_terminal_responses_event(event: &str, body: &Value) -> bool {
    matches!(
        body.get("type").and_then(Value::as_str).unwrap_or(event),
        "response.completed" | "response.failed" | "response.incomplete" | "error"
    )
}

fn terminal_billing_status(event: &str) -> &'static str {
    match event {
        "response.completed" => "success",
        "response.incomplete" => "incomplete",
        _ => "error",
    }
}

fn response_billing_status(body: &Value) -> &'static str {
    match body.get("status").and_then(Value::as_str) {
        Some("completed" | "queued" | "in_progress") => "success",
        Some("failed" | "cancelled") => "error",
        Some("incomplete") => "incomplete",
        _ => "error",
    }
}

fn response_model(body: &Value) -> Option<&str> {
    body.pointer("/response/model")
        .or_else(|| body.get("model"))
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
}

fn effective_response_affinity_model(
    actual_model: Option<&str>,
    ctx: &RequestContext,
) -> Option<String> {
    actual_model
        .filter(|model| !model.trim().is_empty())
        .or_else(|| (!ctx.model.trim().is_empty()).then_some(ctx.model.as_str()))
        .map(str::to_string)
}

async fn resolved_responses_billing_context(
    state: &AppState,
    ctx: &Arc<RequestContext>,
    actual_model: Option<&str>,
) -> Result<Arc<RequestContext>> {
    if !ctx.model.is_empty() {
        return Ok(Arc::clone(ctx));
    }
    let Some(actual_model) = actual_model else {
        return Ok(Arc::clone(ctx));
    };
    let provider = keycompute_pricing::resolve_pricing_provider(actual_model);
    let pricing = state
        .pricing
        .create_snapshot(actual_model, &ctx.tenant_id, Some(provider))
        .await
        .map_err(|error| {
            ApiError::Internal(format!(
                "Failed to resolve pricing for upstream Responses model: {error}"
            ))
        })?;
    let mut resolved = ctx.as_ref().clone();
    resolved.model = actual_model.to_string();
    resolved.update_pricing(pricing);
    Ok(Arc::new(resolved))
}

fn response_client_outcome(body: &Value) -> ClientResponseOutcome {
    if response_billing_status(body) == "success" {
        ClientResponseOutcome::Succeeded
    } else {
        ClientResponseOutcome::ResponseFailed
    }
}

async fn finalize_responses_billing_logged(
    state: &AppState,
    billing: &keycompute_billing::BillingService,
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    status: &str,
) -> bool {
    finalize_responses_billing_logged_with_tpm_timing(
        state,
        billing,
        ctx,
        provider,
        account_id,
        status,
        ResponsesTpmTiming::LedgerFinishedAt,
    )
    .await
}

async fn finalize_responses_billing_logged_with_tpm_timing(
    state: &AppState,
    billing: &keycompute_billing::BillingService,
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    status: &str,
    tpm_timing: ResponsesTpmTiming,
) -> bool {
    let (provider, account_id) = ctx.billing_target(provider, account_id);
    let usage_log = match billing
        .finalize_and_save(ctx, &provider, account_id, status)
        .await
    {
        Ok(usage_log) => usage_log,
        Err(error) => {
            tracing::error!(request_id = %ctx.request_id, %error, "failed to finalize Responses billing");
            return false;
        }
    };
    if let Err(error) = billing
        .replay_saved_usage_effects(ctx, &usage_log, ctx.user_id)
        .await
    {
        tracing::error!(request_id = %ctx.request_id, %error, "failed to apply Responses post-ledger effects");
        return defer_responses_post_ledger_failure(
            state,
            ctx,
            &provider,
            account_id,
            status,
            tpm_timing,
            "post-ledger effects",
        )
        .await;
    }
    let input_tokens = u32::try_from(usage_log.input_tokens).unwrap_or_default();
    let output_tokens = u32::try_from(usage_log.output_tokens).unwrap_or_default();
    if let Err(error) = record_responses_token_usage_for_timing(
        state,
        ctx,
        input_tokens.saturating_add(output_tokens),
        tpm_timing,
        usage_log.finished_at,
    )
    .await
    {
        tracing::warn!(
            request_id = %ctx.request_id,
            %error,
            "failed to record Responses token usage for TPM limiting"
        );
        return defer_responses_post_ledger_failure(
            state,
            ctx,
            &provider,
            account_id,
            status,
            tpm_timing,
            "TPM accounting",
        )
        .await;
    }
    acknowledge_terminal_settlement_outbox(state, ctx).await;
    true
}

async fn defer_responses_post_ledger_failure(
    state: &AppState,
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    status: &str,
    tpm_timing: ResponsesTpmTiming,
    failed_step: &str,
) -> bool {
    let durable = persist_terminal_responses_outbox_with_tpm_timing(
        state,
        ctx,
        provider,
        account_id,
        None,
        false,
        Some(ctx.model.clone()),
        status,
        tpm_timing,
    )
    .await;
    if !durable {
        tracing::error!(
            request_id = %ctx.request_id,
            failed_step,
            "Responses ledger committed but deferred settlement could not be persisted"
        );
    }
    durable
}

#[cfg(test)]
async fn record_responses_token_usage_values(
    state: &AppState,
    ctx: &RequestContext,
    total_tokens: u32,
) -> keycompute_types::Result<()> {
    record_responses_token_usage_values_at(state, ctx, total_tokens, std::time::SystemTime::now())
        .await
}

async fn record_responses_token_usage_values_at(
    state: &AppState,
    ctx: &RequestContext,
    total_tokens: u32,
    occurred_at: std::time::SystemTime,
) -> keycompute_types::Result<()> {
    super::record_terminal_token_usage_at(
        state.rate_limiter.as_ref(),
        ctx,
        total_tokens,
        occurred_at,
    )
    .await
}

async fn record_responses_token_usage_for_timing(
    state: &AppState,
    ctx: &RequestContext,
    total_tokens: u32,
    timing: ResponsesTpmTiming,
    ledger_finished_at: chrono::DateTime<chrono::Utc>,
) -> keycompute_types::Result<()> {
    let terminal_at = match timing {
        ResponsesTpmTiming::LedgerFinishedAt => ledger_finished_at,
        ResponsesTpmTiming::TerminalAt(terminal_at) => terminal_at,
        ResponsesTpmTiming::Skip => return Ok(()),
    };
    record_responses_token_usage_values_if_fresh(state, ctx, total_tokens, terminal_at).await
}

async fn record_responses_token_usage_values_if_fresh(
    state: &AppState,
    ctx: &RequestContext,
    total_tokens: u32,
    terminal_at: chrono::DateTime<chrono::Utc>,
) -> keycompute_types::Result<()> {
    let window = chrono::Duration::seconds(
        i64::try_from(keycompute_ratelimit::WINDOW_SECS).unwrap_or(i64::MAX),
    );
    if chrono::Utc::now().signed_duration_since(terminal_at) >= window {
        return Ok(());
    }
    record_responses_token_usage_values_at(state, ctx, total_tokens, terminal_at.into()).await
}

#[derive(Debug, Clone)]
struct ResponsesIdempotency {
    binding_id: String,
    request_fingerprint: String,
    billing_request_id: uuid::Uuid,
}

#[derive(Debug, Clone)]
struct ResponsesIdempotencyExecution {
    tenant_id: uuid::Uuid,
    binding_id: String,
    execution_token: uuid::Uuid,
    newly_bound: bool,
}

enum ResponsesIdempotencyBinding {
    Execute {
        account: ResolvedResponsesAccount,
        execution: ResponsesIdempotencyExecution,
    },
    Replay(CachedResponsesIdempotencyResult),
}

struct CachedResponsesIdempotencyResult {
    response: ClientUpstreamResponse,
    admission: Option<LargeBodyPermit>,
}

fn require_no_completed_responses_idempotency_ledger(ledger_exists: bool) -> Result<()> {
    if !ledger_exists {
        return Ok(());
    }
    Err(ApiError::Conflict(
        "The original idempotent Responses result is no longer replayable".to_string(),
    ))
}

fn responses_idempotency_claim_matches(
    claim: &ResponsesIdempotencyClaim,
    idempotency: &ResponsesIdempotency,
    user_id: uuid::Uuid,
    produce_ai_key_id: uuid::Uuid,
) -> bool {
    claim.request_fingerprint == idempotency.request_fingerprint
        && claim.billing_request_id == idempotency.billing_request_id
        && claim.user_id == user_id
        && claim.produce_ai_key_id == produce_ai_key_id
}

fn responses_idempotency_metadata_matches(
    claim: &ResponsesIdempotencyClaimMetadata,
    idempotency: &ResponsesIdempotency,
    user_id: uuid::Uuid,
    produce_ai_key_id: uuid::Uuid,
) -> bool {
    claim.request_fingerprint == idempotency.request_fingerprint
        && claim.billing_request_id == idempotency.billing_request_id
        && claim.user_id == user_id
        && claim.produce_ai_key_id == produce_ai_key_id
}

fn cached_responses_idempotency_body_bytes(
    claim: &ResponsesIdempotencyClaimMetadata,
) -> Result<Option<usize>> {
    match claim.execution_state.as_str() {
        "in_progress" => Ok(None),
        "expired" => Err(ApiError::Conflict(
            "The original idempotent Responses result is no longer replayable".to_string(),
        )),
        "completed" => {
            if claim
                .response_expires_at
                .is_none_or(|expires_at| expires_at <= chrono::Utc::now())
            {
                return Err(ApiError::Conflict(
                    "The original idempotent Responses result is no longer replayable".to_string(),
                ));
            }
            let body_bytes = claim.response_body_bytes.ok_or_else(|| {
                ApiError::Internal("Stored idempotent Responses body is missing".to_string())
            })?;
            usize::try_from(body_bytes).map(Some).map_err(|_| {
                ApiError::Internal("Stored idempotent Responses body size is invalid".to_string())
            })
        }
        _ => Err(ApiError::Internal(
            "Stored idempotent Responses state is invalid".to_string(),
        )),
    }
}

fn cached_responses_idempotency_result(
    mut claim: ResponsesIdempotencyClaim,
    admission: Option<LargeBodyPermit>,
) -> Result<Option<CachedResponsesIdempotencyResult>> {
    match claim.execution_state.as_str() {
        "in_progress" => Ok(None),
        "expired" => Err(ApiError::Conflict(
            "The original idempotent Responses result is no longer replayable".to_string(),
        )),
        "completed" => {
            if claim
                .response_expires_at
                .is_none_or(|expires_at| expires_at <= chrono::Utc::now())
            {
                return Err(ApiError::Conflict(
                    "The original idempotent Responses result is no longer replayable".to_string(),
                ));
            }
            let status = claim
                .response_status
                .and_then(|status| u16::try_from(status).ok())
                .ok_or_else(|| {
                    ApiError::Internal("Stored idempotent Responses status is invalid".to_string())
                })?;
            let headers =
                serde_json::from_value(claim.response_headers.take().ok_or_else(|| {
                    ApiError::Internal(
                        "Stored idempotent Responses headers are missing".to_string(),
                    )
                })?)
                .map_err(|error| {
                    ApiError::Internal(format!(
                        "Stored idempotent Responses headers are invalid: {error}"
                    ))
                })?;
            let body = claim.response_body.take().ok_or_else(|| {
                ApiError::Internal("Stored idempotent Responses body is missing".to_string())
            })?;
            Ok(Some(CachedResponsesIdempotencyResult {
                response: ClientUpstreamResponse {
                    status,
                    headers,
                    body,
                },
                admission,
            }))
        }
        _ => Err(ApiError::Internal(
            "Stored idempotent Responses state is invalid".to_string(),
        )),
    }
}

async fn replay_completed_responses_idempotency(
    state: &AppState,
    tenant_id: uuid::Uuid,
    user_id: uuid::Uuid,
    produce_ai_key_id: uuid::Uuid,
    idempotency: &ResponsesIdempotency,
) -> Result<Option<CachedResponsesIdempotencyResult>> {
    let pool = state.pool.as_deref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Idempotent Responses requests require durable state".to_string(),
        )
    })?;
    let Some(metadata) = ResponsesIdempotencyClaim::find_metadata_for_key_share(
        pool,
        tenant_id,
        &idempotency.binding_id,
    )
    .await
    .map_err(|error| responses_state_unavailable("load Responses idempotency state", error))?
    else {
        return Ok(None);
    };
    if !responses_idempotency_metadata_matches(&metadata, idempotency, user_id, produce_ai_key_id) {
        return Err(ApiError::Conflict(
            "The Idempotency-Key was already used with a different request or credential"
                .to_string(),
        ));
    }
    let Some(body_bytes) = cached_responses_idempotency_body_bytes(&metadata)? else {
        return Ok(None);
    };
    let admission = admit_cached_responses_body(body_bytes, try_acquire_large_body_permit)?;
    let claim =
        ResponsesIdempotencyClaim::find_for_key_share(pool, tenant_id, &idempotency.binding_id)
            .await
            .map_err(|error| {
                responses_state_unavailable("load Responses idempotency result", error)
            })?
            .ok_or_else(|| {
                ApiError::Internal("Stored idempotent Responses claim disappeared".to_string())
            })?;
    if !responses_idempotency_claim_matches(&claim, idempotency, user_id, produce_ai_key_id) {
        return Err(ApiError::Conflict(
            "The Idempotency-Key was already used with a different request or credential"
                .to_string(),
        ));
    }
    cached_responses_idempotency_result(claim, admission)
}

fn admit_cached_responses_body<T>(
    body_bytes: usize,
    try_acquire: impl FnOnce() -> Option<T>,
) -> Result<Option<T>> {
    if body_bytes <= LARGE_JSON_BODY_ADMISSION_BYTES {
        return Ok(None);
    }
    try_acquire().map(Some).ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Responses replay capacity is exhausted. Please try again later.".to_string(),
        )
    })
}

fn responses_idempotency(
    headers: &HeaderMap,
    request_path: &str,
    tenant_id: uuid::Uuid,
    body: &Value,
) -> Result<Option<ResponsesIdempotency>> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let key = value.to_str().map_err(|_| {
        ApiError::BadRequest("Idempotency-Key must contain visible UTF-8 text".to_string())
    })?;
    if key.is_empty() || key.len() > 256 || key.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "Idempotency-Key must contain between 1 and 256 visible characters".to_string(),
        ));
    }

    let key_hash = Sha256::digest(key.as_bytes());
    let binding_id = format!("kc_idempotency_{key_hash:x}");
    let mut fingerprint_hasher = Sha256::new();
    fingerprint_hasher.update(request_path.as_bytes());
    fingerprint_hasher.update([0]);
    if let Some(beta) = headers.get("openai-beta") {
        fingerprint_hasher.update(beta.as_bytes());
    }
    fingerprint_hasher.update([0]);
    serde_json::to_writer(Sha256Writer(&mut fingerprint_hasher), &CanonicalJson(body)).map_err(
        |error| {
            ApiError::Internal(format!(
                "Failed to hash canonical Responses request JSON: {error}"
            ))
        },
    )?;
    let request_fingerprint = format!("{:x}", fingerprint_hasher.finalize());

    // Derive a stable opaque UUID without retaining the plaintext key. Set the
    // RFC 4122 variant/version bits so it remains a conventional UUID value.
    let mut billing_hasher = Sha256::new();
    billing_hasher.update(b"keycompute-responses-billing-id-v1");
    billing_hasher.update(tenant_id.as_bytes());
    billing_hasher.update(key_hash);
    let digest = billing_hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    Ok(Some(ResponsesIdempotency {
        binding_id,
        request_fingerprint,
        billing_request_id: uuid::Uuid::from_bytes(bytes),
    }))
}

struct Sha256Writer<'a>(&'a mut Sha256);

impl std::io::Write for Sha256Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct CanonicalJson<'a>(&'a Value);

impl Serialize for CanonicalJson<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0 {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(value) => serializer.serialize_bool(*value),
            Value::Number(value) => value.serialize(serializer),
            Value::String(value) => serializer.serialize_str(value),
            Value::Array(values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(&CanonicalJson(value))?;
                }
                sequence.end()
            }
            Value::Object(object) => {
                let mut fields = object.iter().collect::<Vec<_>>();
                fields.sort_unstable_by_key(|(name, _)| *name);
                let mut map = serializer.serialize_map(Some(fields.len()))?;
                for (name, value) in fields {
                    map.serialize_entry(name, &CanonicalJson(value))?;
                }
                map.end()
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn bind_responses_idempotency(
    state: &AppState,
    tenant_id: uuid::Uuid,
    user_id: uuid::Uuid,
    produce_ai_key_id: uuid::Uuid,
    idempotency: &ResponsesIdempotency,
    requested_model: &str,
    selected_provider: &str,
    selected_account_id: uuid::Uuid,
) -> Result<ResponsesIdempotencyBinding> {
    let pool = state.pool.as_deref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Idempotent Responses requests require durable state".to_string(),
        )
    })?;
    let txn = pool.begin().await.map_err(|error| {
        responses_state_unavailable("begin a Responses idempotency claim", error)
    })?;
    let execution_token = uuid::Uuid::new_v4();
    let lease_seconds = state
        .gateway_config
        .timeout_secs
        .saturating_add(60)
        .max(5 * 60);
    let lease_expires_at = chrono::Utc::now()
        + chrono::Duration::seconds(i64::try_from(lease_seconds).unwrap_or(i64::MAX));
    let (binding, inserted) = ResponsesIdempotencyClaim::bind_for_execution_metadata(
        &txn,
        tenant_id,
        &idempotency.binding_id,
        &idempotency.request_fingerprint,
        idempotency.billing_request_id,
        user_id,
        produce_ai_key_id,
        selected_provider,
        Some(requested_model),
        selected_account_id,
        execution_token,
        lease_expires_at,
    )
    .await
    .map_err(map_responses_idempotency_bind_error)?;
    if !responses_idempotency_metadata_matches(&binding, idempotency, user_id, produce_ai_key_id) {
        return Err(ApiError::Conflict(
            "The Idempotency-Key was already used with a different request or credential"
                .to_string(),
        ));
    }
    if matches!(binding.execution_state.as_str(), "completed" | "expired") {
        txn.commit().await.map_err(|error| {
            responses_state_unavailable("commit a Responses idempotency replay", error)
        })?;
        let cached = replay_completed_responses_idempotency(
            state,
            tenant_id,
            user_id,
            produce_ai_key_id,
            idempotency,
        )
        .await?
        .ok_or_else(|| {
            ApiError::Internal(
                "Stored idempotent Responses result disappeared before replay".to_string(),
            )
        })?;
        return Ok(ResponsesIdempotencyBinding::Replay(cached));
    }
    if binding.execution_state != "in_progress" {
        return Err(ApiError::Internal(
            "Stored idempotent Responses state is invalid".to_string(),
        ));
    }
    if !inserted && binding.upstream_dispatched_at.is_some() {
        return Err(ApiError::Conflict(
            "The original idempotent Responses request was dispatched but its response is not replayable"
                .to_string(),
        ));
    }
    if !inserted && binding.lease_expires_at > chrono::Utc::now() {
        return Err(ApiError::Conflict(
            "The original idempotent Responses request is still executing".to_string(),
        ));
    }

    let ledger_exists =
        keycompute_db::UsageLog::exists_by_billing_request_id(&txn, idempotency.billing_request_id)
            .await
            .map_err(|error| {
                responses_state_unavailable("inspect Responses idempotent billing", error)
            })?;
    // A recreated claim must never execute after the immutable billing ledger
    // proves that the same logical request reached settlement without leaving
    // a replayable terminal HTTP response.
    require_no_completed_responses_idempotency_ledger(ledger_exists)?;

    if !inserted {
        ResponsesIdempotencyClaim::reclaim_expired_execution(
            &txn,
            tenant_id,
            &idempotency.binding_id,
            execution_token,
            lease_expires_at,
        )
        .await
        .map_err(|error| responses_state_unavailable("reclaim Responses idempotency key", error))?
        .ok_or_else(|| {
            ApiError::Conflict(
                "The original idempotent Responses request is still executing".to_string(),
            )
        })?;
    }

    let account = Account::find_by_id_for_key_share(&txn, binding.account_id)
        .await
        .map_err(|error| responses_state_unavailable("load idempotent Responses account", error))?
        .ok_or_else(|| {
            ApiError::ServiceUnavailable(
                "The idempotent Responses account is no longer available".to_string(),
            )
        })?;
    // Reclamation is possible only while `upstream_dispatched_at` is NULL, so
    // the abandoned execution cannot have produced upstream side effects. Use
    // the account's current connection snapshot: the durable reservation held
    // by the caller serializes it with concurrent endpoint/key updates. A
    // general account `updated_at` check would incorrectly reject harmless
    // metadata, limit, or priority edits; current eligibility is revalidated
    // by `reserved_responses_account_snapshot` below.
    let mut resolved =
        reserved_responses_account_snapshot(account, &binding.provider, tenant_id, true)?;
    resolved.model = binding.model;
    txn.commit().await.map_err(|error| {
        responses_state_unavailable("commit a Responses idempotency claim", error)
    })?;
    Ok(ResponsesIdempotencyBinding::Execute {
        account: resolved,
        execution: ResponsesIdempotencyExecution {
            tenant_id,
            binding_id: idempotency.binding_id.clone(),
            execution_token,
            newly_bound: inserted,
        },
    })
}

async fn complete_responses_idempotency_execution(
    state: &AppState,
    execution: &ResponsesIdempotencyExecution,
    response: &ClientUpstreamResponse,
) -> Result<()> {
    let pool = state.pool.as_deref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Idempotent Responses requests require durable state".to_string(),
        )
    })?;
    let status = i16::try_from(response.status)
        .map_err(|_| ApiError::Internal("Responses status cannot be persisted".to_string()))?;
    let headers = serde_json::to_value(&response.headers).map_err(|error| {
        ApiError::Internal(format!(
            "Failed to serialize idempotent Responses headers: {error}"
        ))
    })?;
    let expires_at = chrono::Utc::now()
        + chrono::Duration::from_std(RESPONSES_IDEMPOTENCY_REPLAY_TTL)
            .unwrap_or(chrono::Duration::hours(24));
    let completed = ResponsesIdempotencyClaim::complete_execution_with_quota(
        pool,
        execution.tenant_id,
        &execution.binding_id,
        execution.execution_token,
        status,
        headers,
        &response.body,
        expires_at,
        RESPONSES_IDEMPOTENCY_REPLAYS_PER_TENANT,
        RESPONSES_IDEMPOTENCY_REPLAY_BYTES_PER_TENANT,
    )
    .await
    .map_err(|error| responses_state_unavailable("store Responses idempotency result", error))?;
    if !completed {
        return Err(ApiError::Conflict(
            "The Responses idempotency execution lease was superseded".to_string(),
        ));
    }
    Ok(())
}

async fn mark_responses_idempotency_dispatched(
    state: &AppState,
    execution: &ResponsesIdempotencyExecution,
) -> Result<()> {
    let pool = state.pool.as_deref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Idempotent Responses requests require durable state".to_string(),
        )
    })?;
    let marked = ResponsesIdempotencyClaim::mark_execution_dispatched(
        pool,
        execution.tenant_id,
        &execution.binding_id,
        execution.execution_token,
    )
    .await
    .map_err(|error| responses_state_unavailable("fence Responses upstream dispatch", error))?;
    if !marked {
        return Err(ApiError::Conflict(
            "The Responses idempotency execution lease was superseded".to_string(),
        ));
    }
    Ok(())
}

async fn expire_dispatched_responses_idempotency_execution(
    state: &AppState,
    execution: Option<&ResponsesIdempotencyExecution>,
) {
    let (Some(pool), Some(execution)) = (state.pool.as_deref(), execution) else {
        return;
    };
    match ResponsesIdempotencyClaim::expire_dispatched_execution(
        pool,
        execution.tenant_id,
        &execution.binding_id,
        execution.execution_token,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            binding_id = %execution.binding_id,
            "Responses idempotency execution could not be finalized as non-replayable"
        ),
        Err(error) => tracing::error!(
            binding_id = %execution.binding_id,
            %error,
            "failed to finalize dispatched Responses idempotency execution"
        ),
    }
}

async fn release_responses_idempotency_execution(
    state: &AppState,
    execution: Option<&ResponsesIdempotencyExecution>,
) {
    let (Some(pool), Some(execution)) = (state.pool.as_deref(), execution) else {
        return;
    };
    if let Err(error) = ResponsesIdempotencyClaim::release_execution(
        pool,
        execution.tenant_id,
        &execution.binding_id,
        execution.execution_token,
    )
    .await
    {
        tracing::warn!(%error, "failed to release Responses idempotency execution lease");
    }
}

async fn abandon_unstarted_responses_idempotency_execution(
    state: &AppState,
    execution: &ResponsesIdempotencyExecution,
) {
    if !execution.newly_bound {
        release_responses_idempotency_execution(state, Some(execution)).await;
        return;
    }
    let Some(pool) = state.pool.as_deref() else {
        return;
    };
    if let Err(error) = ResponsesIdempotencyClaim::delete_unstarted_execution(
        pool,
        execution.tenant_id,
        &execution.binding_id,
        execution.execution_token,
    )
    .await
    {
        tracing::warn!(%error, "failed to delete unstarted Responses idempotency claim");
    }
}

fn upstream_responses_idempotency_key(tenant_id: uuid::Uuid, client_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"keycompute-responses-upstream-idempotency-v1");
    hasher.update(tenant_id.as_bytes());
    hasher.update([0]);
    hasher.update(client_key.as_bytes());
    format!("kc_upstream_{:x}", hasher.finalize())
}

fn forwarded_responses_headers(
    headers: &HeaderMap,
    tenant_id: uuid::Uuid,
) -> Result<BTreeMap<String, String>> {
    let mut forwarded = BTreeMap::new();
    for name in ["idempotency-key", "openai-beta"] {
        if let Some(value) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
        {
            let value = if name == "idempotency-key" {
                upstream_responses_idempotency_key(tenant_id, value)
            } else {
                value.to_string()
            };
            forwarded.insert(name.to_string(), value);
        }
    }
    if let Some(value) = headers.get("x-client-request-id") {
        let value = value.to_str().map_err(|_| {
            ApiError::BadRequest(
                "X-Client-Request-Id must contain no more than 512 ASCII characters".to_string(),
            )
        })?;
        if value.is_empty()
            || value.len() > 512
            || !value.is_ascii()
            || value.chars().any(char::is_control)
        {
            return Err(ApiError::BadRequest(
                "X-Client-Request-Id must contain between 1 and 512 visible ASCII characters"
                    .to_string(),
            ));
        }
        forwarded.insert("x-client-request-id".to_string(), value.to_string());
    }
    Ok(forwarded)
}

fn context_messages(body: &Value) -> Vec<Message> {
    let mut messages = Vec::new();
    if let Some(instructions) = body.get("instructions")
        && !instructions.is_null()
    {
        messages.push(Message {
            role: MessageRole::System,
            content: MessageContent::text(project_value(instructions)),
        });
    }
    match body.get("input") {
        Some(Value::String(text)) => messages.push(Message {
            role: MessageRole::User,
            content: MessageContent::text(text.clone()),
        }),
        Some(Value::Array(items)) => {
            for item in items {
                let role = match item.get("role").and_then(Value::as_str) {
                    Some("assistant") => MessageRole::Assistant,
                    Some("system") | Some("developer") => MessageRole::System,
                    _ => MessageRole::User,
                };
                messages.push(Message {
                    role,
                    content: MessageContent::text(project_value(item)),
                });
            }
        }
        Some(value) if !value.is_null() => messages.push(Message {
            role: MessageRole::User,
            content: MessageContent::text(project_value(value)),
        }),
        _ => {}
    }
    messages
}

fn project_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(project_value)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text") => object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Some("message") => object
                .get("content")
                .map(project_value)
                .unwrap_or_else(|| "[message]".to_string()),
            Some("input_image" | "computer_screenshot") => "[image]".to_string(),
            Some("input_file") => "[file]".to_string(),
            Some(
                "function_call"
                | "function_call_output"
                | "custom_tool_call"
                | "custom_tool_call_output",
            ) => "[tool]".to_string(),
            Some("reasoning") => "[reasoning]".to_string(),
            Some("item_reference") => "[item_reference]".to_string(),
            _ => object
                .get("content")
                .or_else(|| object.get("text"))
                .map(project_value)
                .unwrap_or_else(|| "[responses_input]".to_string()),
        },
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TerminalUsageCallCounter {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl keycompute_ratelimit::RateLimiter for TerminalUsageCallCounter {
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

        async fn record_tokens(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            _tokens: u32,
        ) -> keycompute_types::Result<()> {
            Ok(())
        }

        async fn record_tokens_once_at(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            _request_id: uuid::Uuid,
            _tokens: u32,
            _occurred_at: std::time::SystemTime,
        ) -> keycompute_types::Result<()> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    async fn terminal_stream_settlement_runs_only_for_an_unsecured_foreground_owner() {
        let calls = std::sync::atomic::AtomicUsize::new(0);

        let mut secured = false;
        assert!(
            secure_responses_stream_settlement_once(true, &mut secured, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(false)
            })
            .await
        );
        assert!(!secured);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        secured = true;
        assert!(
            secure_responses_stream_settlement_once(false, &mut secured, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(false)
            })
            .await
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        secured = false;
        assert!(
            secure_responses_stream_settlement_once(false, &mut secured, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(true)
            })
            .await
        );
        assert!(secured);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        assert!(
            secure_responses_stream_settlement_once(false, &mut secured, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(false)
            })
            .await
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a later generic terminal event must reuse the secured native-event settlement"
        );

        secured = false;
        assert!(
            !secure_responses_stream_settlement_once(false, &mut secured, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(false)
            })
            .await
        );
        assert!(!secured);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    struct AbortAwareReservationCleanup {
        calls: std::sync::atomic::AtomicUsize,
        first_call_started: tokio::sync::Notify,
        deleted: tokio::sync::mpsc::UnboundedSender<(uuid::Uuid, String)>,
    }

    #[async_trait::async_trait]
    impl ResponsesReservationCleanup for AbortAwareReservationCleanup {
        async fn delete(
            &self,
            tenant_id: uuid::Uuid,
            reservation_id: &str,
        ) -> std::result::Result<(), String> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                self.first_call_started.notify_one();
                futures::future::pending::<()>().await;
            }
            self.deleted
                .send((tenant_id, reservation_id.to_string()))
                .map_err(|error| error.to_string())
        }
    }

    #[tokio::test]
    async fn aborting_reservation_release_retries_cleanup_from_drop() {
        let tenant_id = uuid::Uuid::new_v4();
        let reservation_id = "kc_reservation_cancelled".to_string();
        let (deleted_tx, mut deleted_rx) = tokio::sync::mpsc::unbounded_channel();
        let cleanup = Arc::new(AbortAwareReservationCleanup {
            calls: std::sync::atomic::AtomicUsize::new(0),
            first_call_started: tokio::sync::Notify::new(),
            deleted: deleted_tx,
        });
        let first_call_started = cleanup.first_call_started.notified();
        let mut reservations = ResponsesExecutionReservations {
            tenant_id,
            reservation_ids: vec![reservation_id.clone()],
            cleanup: Some(cleanup.clone()),
        };

        let release = tokio::spawn(async move {
            reservations.release().await;
        });
        tokio::time::timeout(Duration::from_secs(1), first_call_started)
            .await
            .expect("the explicit release should enter its delete future");
        release.abort();
        assert!(release.await.unwrap_err().is_cancelled());

        let deleted = tokio::time::timeout(Duration::from_secs(1), deleted_rx.recv())
            .await
            .expect("Drop should schedule a second cleanup attempt")
            .expect("cleanup channel should remain open");
        assert_eq!(deleted, (tenant_id, reservation_id));
        assert_eq!(cleanup.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn response_body_owns_admission_guard_after_head_is_dropped() {
        struct TestGuard(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for TestGuard {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut response = Response::new(Body::from("large response"));
        retain_response_body_guard(&mut response, TestGuard(Arc::clone(&released)));
        assert!(!released.load(std::sync::atomic::Ordering::SeqCst));

        let (head, body) = response.into_parts();
        drop(head);
        assert!(
            !released.load(std::sync::atomic::Ordering::SeqCst),
            "dropping the response head must not release body admission"
        );

        let mut body = body.into_data_stream();
        let chunk = body.next().await.unwrap().unwrap();
        assert_eq!(chunk, bytes::Bytes::from_static(b"large response"));
        assert!(!released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(body.next().await.is_none());
        assert!(
            !released.load(std::sync::atomic::Ordering::SeqCst),
            "an outbound data chunk must retain admission after body EOF"
        );
        drop(chunk);
        assert!(
            released.load(std::sync::atomic::Ordering::SeqCst),
            "body admission should release only after the final chunk is dropped"
        );
    }

    #[test]
    fn passthrough_json_rejects_excessive_working_set_before_deserialization() {
        let body = r#"{"object":"response","output":[0,0,0,0],"status":"completed"}"#;
        let estimated = estimated_json_parse_working_set_bytes(body.as_bytes());
        let mut admission = None;

        let error = admit_responses_json_parse_with_limit(
            body,
            estimated.saturating_sub(1),
            &mut admission,
        )
        .expect_err("a response over the working-set limit must be rejected");

        assert!(matches!(
            error,
            ApiError::Provider(message) if message.contains("working-set limit")
        ));
        assert!(admission.is_none());
    }

    #[tokio::test]
    async fn passthrough_failed_responses_are_sanitized_for_json_and_sse() {
        let failed = json!({
            "id": "resp_failed",
            "object": "response",
            "status": "failed",
            "error": {
                "code": "provider_failure",
                "message": "host=internal.example token=secret",
                "debug": "internal-stack"
            },
            "future_field": true
        });
        let response = passthrough_response(
            llm_protocol_provider::UpstreamResponse {
                meta: llm_protocol_provider::UpstreamResponseMeta::synthetic_success(),
                body: PassthroughBody::Full(
                    llm_protocol_provider::AdmittedResponseText::unadmitted(failed.to_string()),
                ),
            },
            None,
            false,
        )
        .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["future_field"], true);
        assert_eq!(body["error"]["code"], "provider_failure");
        assert_eq!(body["error"]["message"], "Upstream request failed");
        assert!(body["error"].get("debug").is_none());
        assert!(!body.to_string().contains("internal.example"));
        assert!(!body.to_string().contains("secret"));

        let event = json!({
            "type": "response.failed",
            "sequence_number": 2,
            "response": failed,
        });
        let stream: ByteStream = Box::pin(futures::stream::iter([Ok(bytes::Bytes::from(
            format!("event: response.failed\ndata: {event}\n\n"),
        ))]));
        let response = passthrough_response(
            llm_protocol_provider::UpstreamResponse {
                meta: llm_protocol_provider::UpstreamResponseMeta::synthetic_success(),
                body: PassthroughBody::Stream(stream),
            },
            None,
            false,
        )
        .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("event: response.failed"));
        assert!(body.contains("Upstream request failed"));
        assert!(body.contains("provider_failure"));
        assert!(!body.contains("internal.example"));
        assert!(!body.contains("secret"));
        assert!(!body.contains("internal-stack"));
    }

    #[tokio::test]
    async fn only_resumed_passthrough_streams_accept_an_empty_body() {
        fn empty_stream() -> ByteStream {
            Box::pin(futures::stream::empty::<
                std::result::Result<bytes::Bytes, keycompute_types::KeyComputeError>,
            >())
        }

        let mut resumed = sanitize_responses_passthrough_stream(empty_stream(), true);
        assert!(resumed.next().await.is_none());

        let mut strict = sanitize_responses_passthrough_stream(empty_stream(), false);
        assert!(matches!(
            strict.next().await,
            Some(Err(keycompute_types::KeyComputeError::ProviderError(message)))
                if message.contains("terminal")
        ));
    }

    #[test]
    fn stored_warmup_http_replay_uses_large_body_admission() {
        let state = AppState::new();
        let large = usize::try_from(RESPONSES_LARGE_HTTP_BODY_BYTES).unwrap() + 1;
        let mut first = None;
        let mut second = None;
        let mut rejected = None;

        admit_stored_warmup_http_context(&state, large, &mut first).unwrap();
        admit_stored_warmup_http_context(&state, large, &mut second).unwrap();
        assert!(matches!(
            admit_stored_warmup_http_context(&state, large, &mut rejected),
            Err(ApiError::RateLimit(_))
        ));

        drop(first);
        admit_stored_warmup_http_context(&state, large, &mut rejected).unwrap();
    }

    #[test]
    fn cached_idempotency_replay_load_sheds_when_large_body_slots_are_exhausted() {
        assert_eq!(
            admit_cached_responses_body(LARGE_JSON_BODY_ADMISSION_BYTES, || -> Option<()> {
                panic!("small cached bodies must not acquire a large-body slot")
            })
            .unwrap(),
            None
        );
        assert_eq!(
            admit_cached_responses_body(LARGE_JSON_BODY_ADMISSION_BYTES + 1, || Some(7_u8))
                .unwrap(),
            Some(7)
        );
        assert!(matches!(
            admit_cached_responses_body(
                LARGE_JSON_BODY_ADMISSION_BYTES + 1,
                Option::<()>::default,
            ),
            Err(ApiError::ServiceUnavailable(message))
                if message.contains("capacity is exhausted")
        ));
    }

    #[test]
    fn stored_warmup_http_replay_enforces_the_combined_working_set_limit() {
        let state = AppState::new();
        let mut existing_body_permit = state.responses_http_body_admission.try_acquire();

        let error = admit_stored_warmup_http_context(
            &state,
            OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES.saturating_add(1),
            &mut existing_body_permit,
        )
        .expect_err("an existing body permit must not bypass the combined hard limit");

        assert!(matches!(
            error,
            ApiError::BadRequest(message) if message.contains("continuation context")
        ));
    }

    #[test]
    fn execution_reservation_ttl_covers_the_longest_stream_timeout() {
        let mut config = keycompute_config::GatewayConfig {
            timeout_secs: 120,
            request_timeout_secs: 300,
            stream_timeout_secs: 3 * 60 * 60,
            ..Default::default()
        };

        assert_eq!(
            responses_execution_reservation_ttl(&config),
            Duration::from_secs(3 * 60 * 60 + 60)
        );

        config.stream_timeout_secs = 600;
        assert_eq!(
            responses_execution_reservation_ttl(&config),
            RESPONSES_EXECUTION_RESERVATION_MIN_TTL
        );
    }

    #[tokio::test]
    async fn local_stored_response_retrieval_honors_stream_and_cursor() {
        let response = local_response_stream(
            json!({
                "id": "resp_ws_local",
                "object": "response",
                "status": "completed",
                "completed_at": 1,
                "output": [],
            }),
            Some("stream=true&starting_after=0"),
        )
        .unwrap();
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "text/event-stream"
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(!body.contains("event: response.created\n"));
        assert!(body.contains("event: response.in_progress\n"));
        assert!(body.contains("event: response.completed\n"));
        assert!(body.contains(r#""sequence_number":2"#));

        assert!(local_response_stream(json!({}), Some("stream=true&starting_after=bad")).is_err());
    }

    #[test]
    fn routing_parser_preserves_full_surface_but_validates_control_fields() {
        let body = json!({
            "model": "gpt-5.1",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "hello"},
                    {"type": "input_image", "image_url": "data:image/png;base64,SECRET"}
                ]
            }],
            "tools": [{"type": "function", "name": "lookup", "parameters": {}}],
            "reasoning": {"effort": "high"},
            "stream": true,
            "max_output_tokens": 100,
        });
        let parsed = ResponsesRoutingFields::parse(&body).unwrap();
        assert_eq!(parsed.model, "gpt-5.1");
        assert!(parsed.stream);
        assert_eq!(parsed.max_output_tokens, Some(100));
        let projected = parsed.messages[0].content.to_string();
        assert!(projected.contains("hello"));
        assert!(projected.contains("[image]"));
        assert!(!projected.contains("SECRET"));
        assert!(body.get("tools").is_some());
        assert!(body.get("reasoning").is_some());
    }

    #[test]
    fn nullable_responses_flags_use_their_protocol_defaults() {
        let body = json!({
            "model": "gpt-5.1",
            "input": "hello",
            "stream": null,
            "store": null,
        });

        let parsed = ResponsesRoutingFields::parse(&body).unwrap();
        assert!(!parsed.stream);
        assert!(response_store_enabled(&body));
        validate_responses_request(&body).unwrap();
    }

    #[test]
    fn compaction_results_never_create_visible_response_affinities() {
        assert!(response_affinity_storage_enabled("/responses", &json!({})));
        assert!(!response_affinity_storage_enabled(
            "/responses",
            &json!({"store": false})
        ));
        assert!(!response_affinity_storage_enabled(
            "/responses/compact",
            &json!({})
        ));
        assert!(!response_affinity_storage_enabled(
            "/responses/compact",
            &json!({"store": true})
        ));
    }

    #[test]
    fn resource_ids_are_opaque_and_resource_kind_controls_retention() {
        assert_eq!(
            conversation_resource_id(&json!({"conversation": "conv_string"})),
            Some("conv_string")
        );
        assert_eq!(
            conversation_resource_id(&json!({"conversation": {"id": "conv_object"}})),
            Some("conv_object")
        );
        assert_eq!(
            conversation_resource_id(&json!({
                "response": {"conversation": {"id": "conv_event"}}
            })),
            Some("conv_event")
        );
        let conflicting_references = json!({
            "previous_response_id": "resp_parent",
            "conversation": "conv_conflict",
        });
        assert!(validate_responses_reference_fields(&conflicting_references).is_err());
        assert!(ResponsesRoutingFields::parse(&conflicting_references).is_err());
        assert_eq!(
            conversation_resource_id(&json!({"conversation": "future.conversation/id:1"})),
            Some("future.conversation/id:1")
        );
        assert_eq!(
            response_resource_id(&json!({"id": "future.response/id:1"})),
            Some("future.response/id:1")
        );
        assert!(conversation_resource_id(&json!({"conversation": ""})).is_none());
        assert_eq!(
            affinity_expiry_unix(ResponsesResourceKind::Conversation, 1),
            chrono::DateTime::<chrono::Utc>::MAX_UTC.timestamp()
        );
        assert_eq!(
            affinity_expiry_unix(ResponsesResourceKind::Response, 1),
            1 + i64::try_from(RESPONSES_AFFINITY_TTL.as_secs()).unwrap()
        );
        assert_eq!(
            upstream_resource_url(
                "https://api.example.test/v1/",
                "responses",
                "future.response/id ?#1",
                "/cancel",
            ),
            "https://api.example.test/v1/responses/future%2Eresponse%2Fid%20%3F%231/cancel"
        );
    }

    #[test]
    fn project_scoped_responses_resources_are_detected_without_custom_schema_false_positives() {
        let cases = vec![
            (json!({"prompt": {"id": "pmpt_owner"}}), "prompt.id"),
            (
                json!({
                    "prompt_cache_options": {"comparison_response_id": "resp_owner"}
                }),
                "prompt_cache_options.comparison_response_id",
            ),
            (
                json!({
                    "input": [{
                        "type": "message",
                        "content": [{"type": "input_file", "file_id": "file-owner"}]
                    }]
                }),
                "input_file.file_id",
            ),
            (
                json!({
                    "tools": [{"type": "file_search", "vector_store_ids": ["vs_owner"]}]
                }),
                "file_search.vector_store_ids",
            ),
            (
                json!({
                    "tools": [{
                        "type": "code_interpreter",
                        "container": {"type": "auto", "file_ids": ["file-owner"]}
                    }]
                }),
                "code_interpreter.container.file_ids",
            ),
            (
                json!({
                    "tools": [{
                        "type": "shell",
                        "environment": {
                            "type": "container_reference",
                            "container_id": "cntr_owner"
                        }
                    }]
                }),
                "input container_id",
            ),
            (
                json!({"input": [{"type": "item_reference", "id": "item_owner"}]}),
                "input item_reference.id",
            ),
        ];
        for (body, resource_path) in cases {
            assert_eq!(
                project_scoped_responses_resource(&body),
                Some(resource_path),
                "failed to detect {resource_path} in {body}"
            );
        }

        assert_eq!(
            project_scoped_responses_resource(&json!({
                "input": [{
                    "type": "message",
                    "content": [{"type": "input_file", "file_url": "https://example.com/a.pdf"}]
                }],
                "tools": [{
                    "type": "function",
                    "name": "custom",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "attachment": {"type": "input_file", "file_id": "not-a-resource"}
                        }
                    }
                }]
            })),
            None
        );
    }

    #[test]
    fn idempotent_project_resources_require_a_resolved_account_owner() {
        let body = json!({"prompt": {"id": "pmpt_owner"}});

        assert!(validate_idempotent_project_resource_owner(&body, false, false).is_ok());
        assert!(validate_idempotent_project_resource_owner(&body, true, true).is_ok());
        assert!(
            validate_idempotent_project_resource_owner(&json!({"input": "inline"}), true, false)
                .is_ok()
        );
        assert!(matches!(
            validate_idempotent_project_resource_owner(&body, true, false),
            Err(ApiError::BadRequest(message))
                if message.contains("prompt.id")
                    && message.contains("previous_response_id or conversation")
        ));
    }

    #[test]
    fn project_resources_require_an_unambiguous_account_route() {
        let body = json!({"prompt": {"id": "pmpt_owner"}});
        let primary = ExecutionTarget::new_provider(
            "openai",
            uuid::Uuid::new_v4(),
            "https://primary.example/v1",
            "primary-key",
        );
        let single_account = ExecutionPlan::new(primary.clone());
        assert!(validate_project_resource_route(&body, &single_account).is_ok());

        let repeated_same_account = single_account.clone().with_fallback(primary);
        assert!(
            validate_project_resource_route(&body, &repeated_same_account).is_ok(),
            "retry copies of one account do not make resource ownership ambiguous"
        );

        let multiple_accounts = single_account.with_fallback(ExecutionTarget::new_provider(
            "openai",
            uuid::Uuid::new_v4(),
            "https://fallback.example/v1",
            "fallback-key",
        ));
        assert!(matches!(
            validate_project_resource_route(&body, &multiple_accounts),
            Err(ApiError::BadRequest(message))
                if message.contains("prompt.id")
                    && message.contains("previous_response_id or conversation")
        ));
        assert!(
            validate_project_resource_route(
                &json!({"model": "gpt-test", "input": "inline"}),
                &multiple_accounts,
            )
            .is_ok()
        );
    }

    #[test]
    fn successful_responses_headers_are_allowlisted_and_replayable() {
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_client_upstream_response_headers(vec![
            ("openai-version".to_string(), "2026-01-01".to_string()),
            (
                "x-ratelimit-remaining-requests".to_string(),
                "7".to_string(),
            ),
            ("set-cookie".to_string(), "secret=value".to_string()),
            ("invalid header".to_string(), "ignored".to_string()),
        ]);

        let cached = cacheable_responses_success_headers(&ctx);
        assert!(cached.contains(&("content-type".to_string(), "application/json".to_string())));
        assert!(cached.contains(&("openai-version".to_string(), "2026-01-01".to_string())));
        assert!(cached.contains(&(
            "x-ratelimit-remaining-requests".to_string(),
            "7".to_string()
        )));
        assert!(!cached.iter().any(|(name, _)| name == "set-cookie"));

        let mut response = Json(json!({"id": "resp_header_test"})).into_response();
        append_forwarded_upstream_response_headers(
            &mut response,
            ctx.client_upstream_response_headers(),
        );
        assert_eq!(response.headers()["openai-version"], "2026-01-01");
        assert_eq!(response.headers()["x-ratelimit-remaining-requests"], "7");
        assert!(!response.headers().contains_key("set-cookie"));
    }

    #[tokio::test]
    async fn input_tokens_discovers_unknown_conversation_before_model_routing() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            tenant_id,
            uuid::Uuid::new_v4(),
            "user",
        );
        let body = json!({"conversation": "conv_unknown", "input": "continue"});
        let routing = ResponsesRoutingFields::parse_input_tokens(&body).unwrap();

        let result = select_responses_post_account(
            &state,
            &auth,
            uuid::Uuid::new_v4(),
            &body,
            routing,
            None,
        )
        .await;

        assert!(matches!(
            result,
            Err(ApiError::ServiceUnavailable(message))
                if message == "Database is required to resolve an unknown conversation"
        ));
    }

    #[tokio::test]
    async fn create_discovers_unknown_conversation_before_pricing_and_model_routing() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            tenant_id,
            uuid::Uuid::new_v4(),
            "user",
        )
        .with_permissions(vec![Permission::UseApi]);

        let result = responses_inner(
            state,
            auth,
            RequestId::new(),
            ClientRequestId(None),
            RequestReceivedAt(chrono::Utc::now()),
            HeaderMap::new(),
            json!({"conversation": "conv_unknown", "input": "continue"}),
            None,
            "/v1/responses",
            "/responses",
            true,
        )
        .await;

        assert!(matches!(
            result,
            Err(ApiError::ServiceUnavailable(message))
                if message == "Database is required to resolve an unknown conversation"
        ));
    }

    #[test]
    fn input_tokens_allows_model_to_come_from_a_previous_response() {
        let chained = json!({
            "previous_response_id": "resp_existing",
            "input": "next turn",
        });
        assert_eq!(
            ResponsesRoutingFields::parse_input_tokens(&chained)
                .unwrap()
                .model,
            ""
        );
        assert_eq!(ResponsesRoutingFields::parse(&chained).unwrap().model, "");
        assert_eq!(
            ResponsesRoutingFields::parse_input_tokens(&json!({"input": "hello"}))
                .unwrap()
                .model,
            ""
        );
    }

    #[test]
    fn compact_requires_an_effective_model_after_affinity_resolution() {
        assert!(validate_effective_responses_model("/responses", "").is_ok());
        assert!(validate_effective_responses_model("/responses/compact", "gpt-test").is_ok());
        assert!(matches!(
            validate_effective_responses_model("/responses/compact", ""),
            Err(ApiError::BadRequest(message))
                if message.contains("model is required")
                    && message.contains("previous_response_id")
        ));
    }

    #[tokio::test]
    async fn compact_without_explicit_or_affinity_model_is_rejected_before_routing() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "user",
        )
        .with_permissions(vec![Permission::UseApi]);

        let result = responses_inner(
            state,
            auth,
            RequestId::new(),
            ClientRequestId(None),
            RequestReceivedAt(chrono::Utc::now()),
            HeaderMap::new(),
            json!({"input": "hello"}),
            None,
            "/v1/responses/compact",
            "/responses/compact",
            false,
        )
        .await;

        assert!(matches!(
            result,
            Err(ApiError::BadRequest(message)) if message.contains("model is required")
        ));
    }

    #[tokio::test]
    async fn streaming_responses_reject_idempotency_keys_before_execution() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "user",
        )
        .with_permissions(vec![Permission::UseApi]);
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "streaming-key".parse().unwrap());

        let result = responses_inner(
            state,
            auth,
            RequestId::new(),
            ClientRequestId(None),
            RequestReceivedAt(chrono::Utc::now()),
            headers,
            json!({"model": "gpt-test", "input": "hello", "stream": true}),
            None,
            "/v1/responses",
            "/responses",
            true,
        )
        .await;

        assert!(matches!(
            result,
            Err(ApiError::BadRequest(message))
                if message.contains("not supported for streaming Responses")
        ));
    }

    #[tokio::test]
    async fn omitted_model_is_resolved_for_billing_from_the_upstream_response() {
        let state = AppState::new();
        let original = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        ));

        let resolved = resolved_responses_billing_context(&state, &original, Some("gpt-resolved"))
            .await
            .unwrap();

        assert_eq!(resolved.model, "gpt-resolved");
        assert!(original.model.is_empty());
    }

    #[test]
    fn replayed_previous_response_id_is_restored_in_json_and_sse_bodies() {
        let mut response = json!({
            "id": "resp_upstream",
            "object": "response",
            "previous_response_id": "resp_upstream_parent",
        });
        patch_client_previous_response_id(&mut response, Some("resp_ws_local"));
        assert_eq!(response["previous_response_id"], "resp_ws_local");

        let mut event = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_upstream",
                "object": "response",
                "previous_response_id": null,
            }
        });
        patch_client_previous_response_id(&mut event, Some("resp_ws_local"));
        assert_eq!(event["response"]["previous_response_id"], "resp_ws_local");
    }

    #[test]
    fn stored_warmup_context_is_expanded_for_cross_transport_continuation() {
        let mut body = json!({
            "previous_response_id": "resp_ws_local",
            "input": "next",
            "tools": [{"type": "function", "name": "new_tool"}],
        });
        apply_stored_warmup_context(
            &mut body,
            StoredWarmupContext {
                items: vec![json!({"type": "message", "role": "user", "content": "first"})],
                request_state: serde_json::Map::from_iter([
                    (
                        "instructions".to_string(),
                        Value::String("remember this".to_string()),
                    ),
                    (
                        "conversation".to_string(),
                        Value::String("conv_warmup".to_string()),
                    ),
                ]),
                upstream_previous_response_id: Some("resp_upstream_parent".to_string()),
                model: Some("gpt-warmup".to_string()),
            },
        )
        .unwrap();

        assert_eq!(body["previous_response_id"], "resp_upstream_parent");
        assert_eq!(body["model"], "gpt-warmup");
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        assert_eq!(body["instructions"], "remember this");
        assert_eq!(body["tools"][0]["name"], "new_tool");
        assert_eq!(conversation_resource_id(&body), None);
    }

    #[test]
    fn stored_warmup_exposes_effective_input_tokens_routing_fields() {
        let mut body = json!({
            "previous_response_id": "resp_ws_local",
            "model": "gpt-explicit",
            "input": "next",
        });
        apply_stored_warmup_context(
            &mut body,
            StoredWarmupContext {
                items: vec![json!({
                    "type": "message",
                    "role": "user",
                    "content": "first"
                })],
                request_state: serde_json::Map::from_iter([(
                    "conversation".to_string(),
                    Value::String("conv_warmup".to_string()),
                )]),
                upstream_previous_response_id: None,
                model: Some("gpt-warmup".to_string()),
            },
        )
        .unwrap();

        let routing = ResponsesRoutingFields::parse_input_tokens(&body).unwrap();
        assert_eq!(routing.model, "gpt-explicit");
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(conversation_resource_id(&body), Some("conv_warmup"));
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        assert!(!local_warmup_has_no_upstream_owner(
            Some("resp_ws_local"),
            &body,
        ));
    }

    #[test]
    fn routing_projection_keeps_large_tool_payloads_out_of_long_lived_messages() {
        let markers = [
            "ARGUMENT_TOKEN".repeat(256),
            "FUNCTION_OUTPUT_TOKEN".repeat(256),
            "CUSTOM_INPUT_TOKEN".repeat(256),
            "CUSTOM_OUTPUT_TOKEN".repeat(256),
        ];
        let routing = ResponsesRoutingFields::parse(&json!({
            "model": "gpt-test",
            "input": [
                {
                    "type": "function_call",
                    "name": "lookup",
                    "call_id": "call_1",
                    "arguments": markers[0],
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": markers[1],
                },
                {
                    "type": "custom_tool_call",
                    "name": "shell",
                    "call_id": "call_2",
                    "input": markers[2],
                },
                {
                    "type": "custom_tool_call_output",
                    "call_id": "call_2",
                    "output": markers[3],
                }
            ]
        }))
        .unwrap();

        assert_eq!(routing.messages.len(), markers.len());
        for message in routing.messages {
            assert_eq!(message.content.extract_text(), "[tool]");
        }
    }

    #[test]
    fn native_events_retain_unknown_fields_without_string_envelopes() {
        let json_event = NativeStreamEvent::OpenAiResponsesJson {
            body: json!({"id":"resp_1","object":"response","future_field":{"x":1}}),
            admission: None,
        };
        let NativeStreamEvent::OpenAiResponsesJson { body, .. } = json_event else {
            panic!("expected Responses JSON event");
        };
        assert_eq!(body["future_field"]["x"], 1);

        let sse_event = NativeStreamEvent::OpenAiResponsesSse {
            event: "response.custom.delta".to_string(),
            data: json!({"type":"response.custom.delta","future":true}),
            admission: None,
        };
        let NativeStreamEvent::OpenAiResponsesSse { event, data, .. } = sse_event else {
            panic!("expected Responses SSE event");
        };
        assert_eq!(event, "response.custom.delta");
        assert_eq!(data["future"], true);
    }

    #[test]
    fn only_root_local_warmups_require_fresh_routing() {
        assert!(local_warmup_has_no_upstream_owner(
            Some("resp_ws_local"),
            &json!({"model":"gpt-next","input":"next"}),
        ));
        assert!(!local_warmup_has_no_upstream_owner(
            Some("resp_ws_local"),
            &json!({
                "model":"gpt-next",
                "previous_response_id":"resp_upstream_parent",
                "input":"next"
            }),
        ));
        assert!(!local_warmup_has_no_upstream_owner(
            Some("resp_ws_local"),
            &json!({"model":"gpt-next","conversation":"conv_123","input":"next"}),
        ));
        assert!(!local_warmup_has_no_upstream_owner(
            None,
            &json!({"model":"gpt-next","input":"next"}),
        ));
    }

    #[tokio::test]
    async fn root_warmup_storage_does_not_require_account_routing() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "user",
        );
        let body = json!({
            "input": "prepare local context",
            "generate": false,
            "store": true
        });
        let routing = ResponsesRoutingFields::parse(&body).unwrap();

        let (provider, owner, reservations) =
            resolve_warmup_storage_owner(&state, &auth, uuid::Uuid::new_v4(), &body, routing, None)
                .await
                .expect("an ownerless root warmup must not consult the empty routing state");

        assert_eq!(provider, "openai");
        assert_eq!(owner, None);
        assert!(reservations.is_empty());
    }

    #[tokio::test]
    async fn chained_warmup_storage_does_not_run_generation_balance_check() {
        let billing_pool =
            keycompute_db::DbRouter::single(sea_orm::DatabaseConnection::Disconnected);
        let mut state = AppState::new();
        state.billing = Arc::new(keycompute_billing::BillingService::with_pool(billing_pool));
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "user",
        );
        let previous_response_id = "resp_upstream_parent";
        state.responses_affinity.write().await.insert(
            affinity_storage_key(auth.tenant_id, previous_response_id),
            ResponsesAffinity {
                tenant_id: auth.tenant_id,
                provider: "openai".to_string(),
                model: Some("gpt-test".to_string()),
                account_id: uuid::Uuid::new_v4(),
                expires_at_unix: chrono::Utc::now().timestamp() + 60,
            },
        );
        let body = json!({
            "model": "gpt-test",
            "input": "prepare continuation",
            "previous_response_id": previous_response_id,
            "generate": false,
            "store": true
        });
        let routing = ResponsesRoutingFields::parse(&body).unwrap();

        let error = match resolve_warmup_storage_owner(
            &state,
            &auth,
            uuid::Uuid::new_v4(),
            &body,
            routing,
            Some(previous_response_id),
        )
        .await
        {
            Ok(_) => panic!("account lookup should still require configured storage"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            ApiError::ServiceUnavailable(message) if message == "Database not configured"
        ));
    }

    #[test]
    fn local_input_items_follow_official_cursor_order_and_limit() {
        let items = vec![
            json!({"id":"item_1","type":"message"}),
            json!({"id":"item_2","type":"message"}),
            json!({"id":"item_3","type":"message"}),
        ];
        let first =
            paginate_local_input_items("resp_local", items.clone(), Some("order=asc&limit=2"))
                .unwrap();
        assert_eq!(first["first_id"], "item_1");
        assert_eq!(first["last_id"], "item_2");
        assert_eq!(first["has_more"], true);
        assert_eq!(first["data"].as_array().unwrap().len(), 2);

        let second = paginate_local_input_items(
            "resp_local",
            items.clone(),
            Some("order=asc&limit=2&after=item_2"),
        )
        .unwrap();
        assert_eq!(second["first_id"], "item_3");
        assert_eq!(second["last_id"], "item_3");
        assert_eq!(second["has_more"], false);

        let descending = paginate_local_input_items("resp_local", items, None).unwrap();
        assert_eq!(descending["first_id"], "item_3");
        assert_eq!(descending["last_id"], "item_1");
        assert!(paginate_local_input_items("resp_local", Vec::new(), Some("limit=0")).is_err());
    }

    #[test]
    fn only_explicit_true_selects_resource_stream_timeout() {
        assert!(response_query_requests_stream(Some(
            "stream=true&starting_after=42"
        )));
        assert!(response_query_requests_stream(Some("stream=TRUE")));
        assert!(!response_query_requests_stream(Some("stream=false")));
        assert!(!response_query_requests_stream(None));

        assert!(response_query_resumes_stream(Some(
            "stream=true&starting_after=42"
        )));
        assert!(!response_query_resumes_stream(Some("stream=true")));
        assert!(!response_query_resumes_stream(Some(
            "stream=false&starting_after=42"
        )));
    }

    #[test]
    fn response_delete_accepts_success_and_already_missing_upstream() {
        assert!(delete_response_is_confirmed(200));
        assert!(delete_response_is_confirmed(204));
        assert!(delete_response_is_confirmed(404));
        assert!(!delete_response_is_confirmed(400));
        assert!(!delete_response_is_confirmed(500));
    }

    #[tokio::test]
    async fn synthesized_delete_response_matches_openai_schema() {
        let response = deleted_response("resp_test");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({
                "id": "resp_test",
                "object": "response",
                "deleted": true,
            })
        );
    }

    #[tokio::test]
    async fn response_delete_without_an_authoritative_route_returns_not_found() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            tenant_id,
            uuid::Uuid::new_v4(),
            "user",
        )
        .with_permissions(vec![Permission::UseApi]);

        let result = delete_response(
            State(state),
            auth,
            Path("resp_missing_route".to_string()),
            HeaderMap::new(),
        )
        .await;

        assert!(matches!(result, Err(ApiError::NotFound(_))));
    }

    #[test]
    fn warmup_storage_quota_is_reported_as_rate_limit() {
        let error = map_response_affinity_write_error(
            keycompute_db::DbError::ResourceLimitExceeded {
                resource: "stored Responses warmups".to_string(),
                limit: "128 entries".to_string(),
            },
            "store Responses warmup",
        );
        assert!(matches!(error, ApiError::RateLimit(_)));
    }

    #[test]
    fn initial_stream_failures_are_returned_before_sse_headers_commit() {
        let error = llm_protocol_provider::StreamEvent::error("connection refused");
        assert_eq!(
            initial_responses_stream_failure(Some(&error))
                .unwrap()
                .into_response()
                .status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            initial_responses_stream_failure(None)
                .unwrap()
                .into_response()
                .status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(
            initial_responses_stream_failure(Some(&llm_protocol_provider::StreamEvent::raw("{}")))
                .is_none()
        );
    }

    #[test]
    fn upstream_stream_error_sanitization_preserves_only_sdk_classification_fields() {
        let mut body = json!({
            "type": "error",
            "code": "rate_limit_exceeded",
            "message": "provider host=internal.example token=secret",
            "param": "model",
            "sequence_number": 7,
            "debug": "debug-token=top-secret",
            "details": {"request": "private prompt"},
            "error": {
                "message": "nested password=secret",
                "trace": "internal-stack"
            },
        });

        assert!(sanitize_upstream_responses_error(Some("error"), &mut body));

        assert_eq!(body["type"], "error");
        assert_eq!(body["code"], "rate_limit_exceeded");
        assert_eq!(body["param"], "model");
        assert_eq!(body["sequence_number"], 7);
        assert_eq!(body["message"], "Upstream request failed");
        assert_eq!(body.as_object().unwrap().len(), 5);
        assert!(body.get("debug").is_none());
        assert!(body.get("details").is_none());
        assert!(body.get("error").is_none());
        assert!(!body.to_string().contains("internal.example"));
        assert!(!body.to_string().contains("secret"));
        assert!(!body.to_string().contains("private prompt"));
    }

    #[test]
    fn upstream_stream_error_sanitization_rejects_untrusted_classification_strings() {
        let mut body = json!({
            "type": "error",
            "code": "sk_live_secret",
            "message": "failed",
            "param": "authorization_token",
            "sequence_number": 3,
        });

        assert!(sanitize_upstream_responses_error(Some("error"), &mut body));

        assert_eq!(body["code"], "server_error");
        assert!(body["param"].is_null());
        assert_eq!(body["message"], "Upstream request failed");
        assert!(!body.to_string().contains("secret"));
        assert!(!body.to_string().contains("authorization_token"));
    }

    #[test]
    fn upstream_failed_response_sanitization_covers_sse_and_json() {
        let mut sse_body = json!({
            "type": "response.failed",
            "sequence_number": 9,
            "response": {
                "id": "resp_failed",
                "object": "response",
                "status": "failed",
                "error": {
                    "type": "server_error",
                    "code": "provider_failure",
                    "message": "provider host=internal.example token=secret",
                    "param": null,
                    "debug": "internal-stack"
                }
            }
        });

        assert!(sanitize_upstream_responses_error(
            Some("response.failed"),
            &mut sse_body
        ));

        assert_eq!(sse_body["response"]["id"], "resp_failed");
        assert_eq!(sse_body["response"]["error"]["type"], "server_error");
        assert_eq!(sse_body["response"]["error"]["code"], "provider_failure");
        assert_eq!(
            sse_body["response"]["error"]["message"],
            "Upstream request failed"
        );
        assert!(sse_body["response"]["error"].get("debug").is_none());
        assert!(!sse_body.to_string().contains("internal.example"));
        assert!(!sse_body.to_string().contains("secret"));

        let mut json_body = json!({
            "id": "resp_failed",
            "object": "response",
            "status": "failed",
            "error": {
                "code": "provider_failure",
                "message": "password=secret",
                "details": {"host": "db.internal"}
            },
            "future_field": true
        });

        assert!(sanitize_upstream_responses_error(None, &mut json_body));

        assert_eq!(json_body["future_field"], true);
        assert_eq!(json_body["error"]["code"], "provider_failure");
        assert_eq!(json_body["error"]["message"], "Upstream request failed");
        assert_eq!(json_body["error"].as_object().unwrap().len(), 2);
        assert!(!json_body.to_string().contains("db.internal"));
        assert!(!json_body.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn final_upstream_stream_error_reaches_the_sse_client_with_safe_fields() {
        let terminal_usage_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut state = AppState::new();
        state.rate_limiter = Arc::new(keycompute_ratelimit::RateLimitService::new(
            Arc::new(TerminalUsageCallCounter {
                calls: Arc::clone(&terminal_usage_calls),
            }),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        ctx.set_input_tokens_estimate(3);
        ctx.set_output_tokens_estimate(2);
        let initial =
            llm_protocol_provider::StreamEvent::native(NativeStreamEvent::OpenAiResponsesSse {
                event: "error".to_string(),
                data: json!({
                    "type": "error",
                    "code": "rate_limit_exceeded",
                    "message": "provider host=internal.example token=secret",
                    "param": "model",
                    "sequence_number": 7,
                    "debug": "debug-token=top-secret",
                    "details": {"request": "private prompt"},
                    "error": {
                        "message": "nested password=secret",
                        "trace": "internal-stack"
                    },
                }),
                admission: None,
            });
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(llm_protocol_provider::StreamEvent::error(
            "terminal executor error",
        ))
        .await
        .unwrap();
        drop(tx);
        let stream = create_responses_stream(
            rx,
            Some(initial),
            ResponsesStreamRuntime {
                ctx,
                provider: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                billing: Arc::clone(&state.billing),
                lifecycle: Arc::clone(&state.lifecycle),
                state,
                client_previous_response_id: None,
                persist_response_affinity: false,
                reservations: ResponsesExecutionReservations::default(),
                body_permit: None,
            },
        );
        let response = Sse::new(stream).into_response();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains("event:error") || body.contains("event: error"));
        assert!(body.contains("rate_limit_exceeded"));
        assert!(body.contains("\"param\":\"model\""));
        assert!(body.contains("\"sequence_number\":7"));
        assert!(body.contains("Upstream request failed"));
        assert!(!body.contains("internal.example"));
        assert!(!body.contains("secret"));
        assert!(!body.contains("private prompt"));
        assert!(!body.contains("internal-stack"));
        assert_eq!(
            terminal_usage_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the native error and its following generic error share one settlement"
        );
    }

    async fn stream_with_unavailable_settlement_storage(terminal_error: bool) -> String {
        let pool = keycompute_db::DbRouter::single(sea_orm::DatabaseConnection::Disconnected);
        let mut state = AppState::with_pool(pool);
        state.lifecycle = Arc::new(NoopRequestLifecycleRecorder);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        ctx.set_input_tokens_estimate(3);
        ctx.set_output_tokens_estimate(2);
        let initial =
            llm_protocol_provider::StreamEvent::native(NativeStreamEvent::OpenAiResponsesSse {
                event: "response.created".to_string(),
                data: json!({
                    "type": "response.created",
                    "response": {
                        "id": "resp_unsettled_stream",
                        "object": "response",
                        "status": "in_progress",
                        "output": []
                    }
                }),
                admission: None,
            });
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(llm_protocol_provider::StreamEvent::native(
            NativeStreamEvent::OpenAiResponsesSse {
                event: "response.output_text.delta".to_string(),
                data: json!({
                    "type": "response.output_text.delta",
                    "delta": "billable output"
                }),
                admission: None,
            },
        ))
        .await
        .unwrap();
        if terminal_error {
            tx.send(llm_protocol_provider::StreamEvent::error(
                "upstream stream failed",
            ))
            .await
            .unwrap();
        }
        drop(tx);
        let stream = create_responses_stream(
            rx,
            Some(initial),
            ResponsesStreamRuntime {
                ctx,
                provider: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                billing: Arc::clone(&state.billing),
                lifecycle: Arc::clone(&state.lifecycle),
                state,
                client_previous_response_id: None,
                persist_response_affinity: false,
                reservations: ResponsesExecutionReservations::default(),
                body_permit: None,
            },
        );
        let response = Sse::new(stream).into_response();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn abnormal_stream_termination_reports_unsecured_billing_state() {
        for terminal_error in [true, false] {
            let body = stream_with_unavailable_settlement_storage(terminal_error).await;
            assert!(body.contains("billable output"));
            assert!(body.contains("Responses billing state could not be durably persisted"));
        }
    }

    #[test]
    fn affinity_cache_limit_applies_to_every_insertion_path() {
        let tenant_id = uuid::Uuid::new_v4();
        let account_id = uuid::Uuid::new_v4();
        let affinity = |expires_at_unix| ResponsesAffinity {
            tenant_id,
            provider: "openai".to_string(),
            model: Some("gpt-test".to_string()),
            account_id,
            expires_at_unix,
        };
        let mut local = std::collections::HashMap::new();
        assert!(insert_response_affinity_with_limit(
            &mut local,
            "expired".to_string(),
            affinity(9),
            0,
            2,
        ));
        assert!(insert_response_affinity_with_limit(
            &mut local,
            "active".to_string(),
            affinity(20),
            0,
            2,
        ));
        assert!(insert_response_affinity_with_limit(
            &mut local,
            "replacement".to_string(),
            affinity(30),
            10,
            2,
        ));
        assert!(!local.contains_key("expired"));
        assert_eq!(local.len(), 2);
        assert!(!insert_response_affinity_with_limit(
            &mut local,
            "overflow".to_string(),
            affinity(40),
            10,
            2,
        ));
        assert_eq!(local.len(), 2);
        assert!(insert_response_affinity_with_limit(
            &mut local,
            "active".to_string(),
            affinity(50),
            10,
            2,
        ));
        assert_eq!(local["active"].expires_at_unix, 50);
    }

    #[tokio::test]
    async fn json_affinity_falls_back_to_requested_model_when_upstream_omits_it() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        let account_id = uuid::Uuid::new_v4();
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            tenant_id,
            uuid::Uuid::new_v4(),
            "gpt-requested",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        ));
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(llm_protocol_provider::StreamEvent::native(
            NativeStreamEvent::OpenAiResponsesJson {
                body: json!({
                    "object": "response",
                    "id": "resp_json_without_model",
                    "status": "completed",
                    "output": []
                }),
                admission: None,
            },
        ))
        .await
        .unwrap();
        tx.send(llm_protocol_provider::StreamEvent::Done)
            .await
            .unwrap();
        drop(tx);

        create_responses_json(
            rx,
            ResponsesJsonRuntime {
                ctx: Arc::clone(&ctx),
                provider: "openai".to_string(),
                account_id,
                billing: Arc::clone(&state.billing),
                lifecycle: Arc::clone(&state.lifecycle),
                state: state.clone(),
                client_previous_response_id: None,
                persist_response_affinity: true,
                reservations: ResponsesExecutionReservations::default(),
                body_permit: None,
                idempotency_execution: None,
            },
        )
        .await
        .unwrap();

        let affinity = response_affinity(&state, "resp_json_without_model", tenant_id)
            .await
            .unwrap();
        assert_eq!(affinity.model.as_deref(), Some("gpt-requested"));
    }

    #[tokio::test]
    async fn compact_json_result_id_is_not_saved_as_a_response_route() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            tenant_id,
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        ));
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(llm_protocol_provider::StreamEvent::native(
            NativeStreamEvent::OpenAiResponsesJson {
                body: json!({
                    "id": "resp_compaction_result",
                    "object": "response.compaction",
                    "output": [],
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                }),
                admission: None,
            },
        ))
        .await
        .unwrap();
        tx.send(llm_protocol_provider::StreamEvent::Done)
            .await
            .unwrap();
        drop(tx);

        create_responses_json(
            rx,
            ResponsesJsonRuntime {
                ctx,
                provider: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                billing: Arc::clone(&state.billing),
                lifecycle: Arc::clone(&state.lifecycle),
                state: state.clone(),
                client_previous_response_id: None,
                persist_response_affinity: response_affinity_storage_enabled(
                    "/responses/compact",
                    &json!({}),
                ),
                reservations: ResponsesExecutionReservations::default(),
                body_permit: None,
                idempotency_execution: None,
            },
        )
        .await
        .unwrap();

        assert!(matches!(
            response_affinity(&state, "resp_compaction_result", tenant_id).await,
            Err(ApiError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn streaming_affinity_is_available_after_response_created() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        let account_id = uuid::Uuid::new_v4();
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            tenant_id,
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let initial =
            llm_protocol_provider::StreamEvent::native(NativeStreamEvent::OpenAiResponsesSse {
                event: "response.created".to_string(),
                data: json!({
                    "type": "response.created",
                    "response": {
                        "id": "resp_stream_pending",
                        "status": "in_progress",
                        "usage": null
                    }
                }),
                admission: None,
            });
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let stream = create_responses_stream(
            rx,
            Some(initial),
            ResponsesStreamRuntime {
                ctx: Arc::clone(&ctx),
                provider: "openai".to_string(),
                account_id,
                billing: Arc::clone(&state.billing),
                lifecycle: Arc::clone(&state.lifecycle),
                state: state.clone(),
                client_previous_response_id: None,
                persist_response_affinity: true,
                reservations: ResponsesExecutionReservations::default(),
                body_permit: None,
            },
        );
        tokio::pin!(stream);

        assert!(stream.next().await.is_some());
        let affinity = response_affinity(&state, "resp_stream_pending", tenant_id)
            .await
            .unwrap();
        assert_eq!(affinity.account_id, account_id);
        assert_eq!(affinity.model.as_deref(), Some("gpt-test"));
        drop(tx);
    }

    #[tokio::test]
    async fn terminal_stream_usage_is_applied_before_the_event_is_visible() {
        let state = AppState::new();
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let terminal =
            llm_protocol_provider::StreamEvent::native(NativeStreamEvent::OpenAiResponsesSse {
                event: "response.completed".to_string(),
                data: json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_terminal_usage",
                        "status": "completed",
                        "usage": {
                            "input_tokens": 17,
                            "output_tokens": 9,
                            "total_tokens": 26
                        }
                    }
                }),
                admission: None,
            });
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let stream = create_responses_stream(
            rx,
            Some(terminal),
            ResponsesStreamRuntime {
                ctx: Arc::clone(&ctx),
                provider: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                billing: Arc::clone(&state.billing),
                lifecycle: Arc::clone(&state.lifecycle),
                state,
                client_previous_response_id: None,
                persist_response_affinity: false,
                reservations: ResponsesExecutionReservations::default(),
                body_permit: None,
            },
        );
        tokio::pin!(stream);

        assert!(stream.next().await.is_some());
        assert_eq!(ctx.usage_snapshot(), (17, 9));

        tx.send(llm_protocol_provider::StreamEvent::Done)
            .await
            .unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn response_affinity_is_tenant_isolated_without_redis() {
        let state = AppState::new();
        let owner = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let account = uuid::Uuid::new_v4();
        save_response_affinity(
            &state,
            "resp_affinity_test",
            ResponsesResourceKind::Response,
            ResponsesAffinityRoute {
                tenant_id: owner,
                provider: "openai".to_string(),
                model: Some("gpt-test".to_string()),
                account_id: account,
            },
            None,
        )
        .await
        .unwrap();

        let affinity = response_affinity(&state, "resp_affinity_test", owner)
            .await
            .unwrap();
        assert_eq!(affinity.account_id, account);
        assert!(matches!(
            response_affinity(&state, "resp_affinity_test", other).await,
            Err(ApiError::NotFound(_))
        ));

        delete_response_affinity(&state, "resp_affinity_test", owner)
            .await
            .unwrap();
        assert!(matches!(
            response_affinity(&state, "resp_affinity_test", owner).await,
            Err(ApiError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn store_false_skips_every_global_response_affinity_layer() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        assert!(!response_store_enabled(&json!({"store": false})));
        assert!(response_store_enabled(&json!({})));

        let durable = save_response_affinity_if_stored(
            &state,
            false,
            "resp_stateless",
            ResponsesAffinityRoute {
                tenant_id,
                provider: "openai".to_string(),
                model: Some("gpt-test".to_string()),
                account_id: uuid::Uuid::new_v4(),
            },
            None,
        )
        .await
        .unwrap();
        let database_less_settlement = save_response_affinity_if_stored(
            &state,
            false,
            "resp_stateless_background",
            ResponsesAffinityRoute {
                tenant_id,
                provider: "openai".to_string(),
                model: Some("gpt-test".to_string()),
                account_id: uuid::Uuid::new_v4(),
            },
            Some(json!({"request_id": uuid::Uuid::new_v4()})),
        )
        .await
        .unwrap();

        assert!(!durable);
        assert!(!database_less_settlement);
        assert!(state.responses_affinity.read().await.is_empty());
        assert!(matches!(
            response_affinity(&state, "resp_stateless", tenant_id).await,
            Err(ApiError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn conversation_affinity_is_tenant_isolated_without_redis() {
        let state = AppState::new();
        let owner = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let account = uuid::Uuid::new_v4();
        save_response_affinity(
            &state,
            "conv_affinity_test",
            ResponsesResourceKind::Conversation,
            ResponsesAffinityRoute {
                tenant_id: owner,
                provider: "openai".to_string(),
                model: Some("gpt-test".to_string()),
                account_id: account,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            response_affinity(&state, "conv_affinity_test", owner)
                .await
                .unwrap()
                .account_id,
            account
        );
        assert!(matches!(
            response_affinity(&state, "conv_affinity_test", other).await,
            Err(ApiError::NotFound(_))
        ));
    }

    #[test]
    fn conversation_discovery_accepts_only_the_requested_conversation_object() {
        assert!(conversation_lookup_matches(
            &json!({"id": "conv_expected", "object": "conversation"}),
            "conv_expected",
        ));
        for body in [
            json!({"id": "conv_other", "object": "conversation"}),
            json!({"id": "conv_expected", "object": "response"}),
            json!({"id": "conv_expected"}),
            json!({"html": "generic compatibility endpoint page"}),
        ] {
            assert!(!conversation_lookup_matches(&body, "conv_expected"));
        }
    }

    #[test]
    fn affinity_routing_revalidates_current_account_visibility() {
        let owner = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let mut account = conversation_test_account(owner, 0);

        assert!(responses_account_is_visible_to_tenant(&account, owner));
        assert!(!responses_account_is_visible_to_tenant(&account, other));

        account.visibility = "global".to_string();
        assert!(responses_account_is_visible_to_tenant(&account, other));

        account.visibility = "tenant".to_string();
        account.tenant_id = other;
        assert!(!responses_account_is_visible_to_tenant(&account, owner));
    }

    #[tokio::test]
    async fn conversation_discovery_never_exceeds_its_probe_budget() {
        let tenant = uuid::Uuid::new_v4();
        let accounts = (0..i32::try_from(RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES).unwrap()
            + 2)
            .map(|priority| conversation_test_account(tenant, priority))
            .collect::<Vec<_>>();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded_attempts = std::sync::Arc::clone(&attempts);

        let result = discover_conversation_account_from_candidates(
            accounts,
            "conv_outside_budget",
            RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES,
            Duration::from_secs(1),
            move |_| {
                let recorded_attempts = std::sync::Arc::clone(&recorded_attempts);
                async move {
                    recorded_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(None)
                }
            },
        )
        .await;

        assert!(
            matches!(result, Err(ApiError::Conflict(message)) if message.contains("8-account"))
        );
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::Relaxed),
            RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES
        );
    }

    #[tokio::test]
    async fn conversation_discovery_continues_after_a_candidate_failure() {
        let tenant = uuid::Uuid::new_v4();
        let candidates = vec![
            conversation_test_account(tenant, 3),
            conversation_test_account(tenant, 2),
            conversation_test_account(tenant, 1),
        ];
        let expected_account_id = candidates[2].id;
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_attempts = std::sync::Arc::clone(&attempts);

        let resolved = discover_conversation_account_from_candidates(
            candidates,
            "conv_owned_by_last_candidate",
            RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES,
            Duration::from_secs(1),
            move |account| {
                let recorded_attempts = std::sync::Arc::clone(&recorded_attempts);
                async move {
                    recorded_attempts.lock().unwrap().push(account.priority);
                    match account.priority {
                        3 => Err(ApiError::Provider("stale credential".to_string())),
                        2 => Ok(None),
                        _ => Ok(Some(ResolvedResponsesAccount {
                            provider: account.provider,
                            model: None,
                            account_id: account.id,
                            endpoint: account.endpoint,
                            api_key: "sk-test".to_string(),
                        })),
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(resolved.account_id, expected_account_id);
        assert_eq!(*attempts.lock().unwrap(), vec![3, 2, 1]);
    }

    #[tokio::test]
    async fn conversation_discovery_returns_the_first_error_when_no_candidate_matches() {
        let tenant = uuid::Uuid::new_v4();
        let candidates = vec![
            conversation_test_account(tenant, 2),
            conversation_test_account(tenant, 1),
        ];

        let result = discover_conversation_account_from_candidates(
            candidates,
            "conv_unavailable",
            RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES,
            Duration::from_secs(1),
            |account| async move {
                if account.priority == 2 {
                    Err(ApiError::Provider("first failure".to_string()))
                } else {
                    Ok(None)
                }
            },
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("conversation discovery unexpectedly resolved an account"),
        };

        assert!(matches!(
            error,
            ApiError::Provider(message) if message == "first failure"
        ));
    }

    #[tokio::test]
    async fn conversation_discovery_has_one_aggregate_timeout() {
        let tenant = uuid::Uuid::new_v4();
        let candidates = vec![
            conversation_test_account(tenant, 2),
            conversation_test_account(tenant, 1),
        ];
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded_attempts = std::sync::Arc::clone(&attempts);

        let result = discover_conversation_account_from_candidates(
            candidates,
            "conv_slow",
            RESPONSES_CONVERSATION_DISCOVERY_MAX_CANDIDATES,
            Duration::from_millis(10),
            move |_| {
                let recorded_attempts = std::sync::Arc::clone(&recorded_attempts);
                async move {
                    recorded_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Ok(None)
                }
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(ApiError::ServiceUnavailable(message))
                if message == "Conversation account discovery timed out; please try again"
        ));
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    fn conversation_test_account(tenant_id: uuid::Uuid, priority: i32) -> Account {
        let now = chrono::Utc::now();
        Account {
            id: uuid::Uuid::new_v4(),
            tenant_id,
            provider: "openai".to_string(),
            name: format!("responses-{priority}"),
            endpoint: "https://example.com/v1".to_string(),
            upstream_api_key_encrypted: "encrypted".to_string(),
            upstream_api_key_preview: "sk-****".to_string(),
            rpm_limit: 60,
            tpm_limit: 100_000,
            priority,
            enabled: true,
            models_supported: vec!["gpt-test".to_string()],
            api_capabilities: vec![AccountApiCapability::Responses.as_str().to_string()],
            visibility: "tenant".to_string(),
            last_probe_at: None,
            last_probe_latency_ms: None,
            last_probe_status: None,
            last_probe_error_code: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn capability_removal_only_excludes_an_account_from_new_responses_routing() {
        let tenant_id = uuid::Uuid::new_v4();
        let mut account = conversation_test_account(tenant_id, 1);
        account.api_capabilities = vec![AccountApiCapability::ChatCompletions.as_str().to_string()];

        assert!(!reserved_responses_account_is_available(&account, true));
        assert!(reserved_responses_account_is_available(&account, false));

        account.enabled = false;
        assert!(!reserved_responses_account_is_available(&account, false));
    }

    #[test]
    fn background_status_and_final_usage_are_accounted_exactly() {
        assert!(response_is_background_pending(&json!({"status": "queued"})));
        assert!(response_is_background_pending(
            &json!({"status": "in_progress"})
        ));
        assert!(!response_is_background_pending(
            &json!({"status": "completed"})
        ));

        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_input_tokens_estimate(1);
        ctx.set_output_tokens_estimate(2);
        apply_response_usage(
            &ctx,
            &json!({"usage": {"input_tokens": 17, "output_tokens": 11}}),
        );
        assert_eq!(ctx.usage_snapshot(), (17, 11));
        assert!(ctx.is_usage_finalized());
    }

    #[test]
    fn background_retrieval_requires_the_requested_valid_resource() {
        let response = json!({
            "id": "resp_expected",
            "object": "response",
            "status": "queued",
            "output": [],
            "usage": null,
        });
        assert_eq!(
            validate_background_response("resp_expected", &response).unwrap(),
            None
        );

        let mut completed = response.clone();
        completed["status"] = Value::String("completed".to_string());
        completed["usage"] = json!({"input_tokens": 7, "output_tokens": 3});
        assert_eq!(
            validate_background_response("resp_expected", &completed).unwrap(),
            Some("success")
        );

        for invalid in [
            json!({}),
            json!({
                "id": "resp_other",
                "object": "response",
                "status": "completed",
                "output": [],
            }),
            json!({
                "id": "resp_expected",
                "object": "response",
                "status": "future_status",
                "output": [],
            }),
            json!({
                "id": "resp_expected",
                "object": "response",
                "status": "completed",
                "output": [],
                "usage": {"input_tokens": 7},
            }),
        ] {
            assert!(
                validate_background_response("resp_expected", &invalid).is_err(),
                "invalid background resource must not become billable: {invalid}"
            );
        }
    }

    #[test]
    fn background_terminal_response_without_usage_estimates_complete_output() {
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_output_tokens_estimate(1);
        let body = json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "complete background response"
                }]
            }],
            "usage": null
        });

        apply_response_usage(&ctx, &body);

        assert_eq!(
            ctx.usage_snapshot().1,
            llm_gateway::estimate_responses_output_tokens(&body)
        );
        assert!(ctx.usage_snapshot().1 > 1);
        assert!(!ctx.is_output_finalized());
    }

    #[test]
    fn durable_background_poll_retries_only_transient_http_statuses() {
        for status in [404, 408, 409, 429, 500, 503] {
            assert!(
                background_poll_status_is_retryable(status),
                "{status} should be retried"
            );
        }
        for status in [400, 401, 403, 422] {
            assert!(
                !background_poll_status_is_retryable(status),
                "{status} should terminate polling"
            );
        }
    }

    #[test]
    fn in_process_background_poll_retries_only_transient_parse_capacity_errors() {
        assert!(background_poll_parse_error_is_retryable(
            &ApiError::ServiceUnavailable(RESPONSES_JSON_PROCESSING_CAPACITY_MESSAGE.to_string())
        ));
        assert!(!background_poll_parse_error_is_retryable(
            &ApiError::Provider("Responses JSON exceeds the working-set limit".to_string())
        ));
        assert!(!background_poll_parse_error_is_retryable(
            &ApiError::ServiceUnavailable("Database not configured".to_string())
        ));
    }

    #[tokio::test]
    async fn completed_responses_accumulate_tpm_usage_across_requests() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        let user_id = uuid::Uuid::new_v4();
        let api_key_id = uuid::Uuid::new_v4();
        for (input_tokens, output_tokens) in [(5, 3), (4, 5)] {
            let ctx = RequestContext::new(
                uuid::Uuid::new_v4(),
                user_id,
                tenant_id,
                api_key_id,
                "gpt-test",
                Vec::new(),
                false,
                keycompute_types::PricingSnapshot::default(),
            );
            ctx.set_input_tokens(input_tokens);
            ctx.set_output_tokens(output_tokens);
            let total_tokens = input_tokens.saturating_add(output_tokens);
            record_responses_token_usage_values(&state, &ctx, total_tokens)
                .await
                .unwrap();
            record_responses_token_usage_values(&state, &ctx, total_tokens)
                .await
                .unwrap();
        }

        let rate_key = RateLimitKey::new(tenant_id, user_id, api_key_id);
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            17
        );
        let config = keycompute_ratelimit::RateLimitConfig::new(60, 16);
        assert!(
            !state
                .rate_limiter
                .check_tpm(&rate_key, &config)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn idempotent_retries_share_one_tpm_record() {
        let state = AppState::new();
        let tenant_id = uuid::Uuid::new_v4();
        let user_id = uuid::Uuid::new_v4();
        let api_key_id = uuid::Uuid::new_v4();
        let billing_request_id = uuid::Uuid::new_v4();
        for _ in 0..2 {
            let mut ctx = RequestContext::new(
                uuid::Uuid::new_v4(),
                user_id,
                tenant_id,
                api_key_id,
                "gpt-test",
                Vec::new(),
                false,
                keycompute_types::PricingSnapshot::default(),
            );
            ctx.set_billing_request_id(billing_request_id);
            ctx.set_input_tokens(5);
            ctx.set_output_tokens(3);
            record_responses_token_usage_values(&state, &ctx, 8)
                .await
                .unwrap();
        }

        let rate_key = RateLimitKey::new(tenant_id, user_id, api_key_id);
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            8
        );
    }

    #[tokio::test]
    async fn in_process_background_billing_uses_provider_completed_at_for_tpm_freshness() {
        let state = AppState::new();
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_input_tokens(7);
        ctx.set_output_tokens(5);
        let completed_at = chrono::Utc::now()
            - chrono::Duration::seconds(
                i64::try_from(keycompute_ratelimit::WINDOW_SECS).unwrap() + 1,
            );
        let terminal_at = background_response_terminal_at(&json!({
            "status": "completed",
            "completed_at": completed_at.timestamp(),
        }))
        .unwrap();
        assert!(
            finalize_responses_billing_logged_with_tpm_timing(
                &state,
                &keycompute_billing::BillingService::new(),
                &ctx,
                "openai",
                uuid::Uuid::new_v4(),
                "success",
                ResponsesTpmTiming::TerminalAt(terminal_at),
            )
            .await
        );

        let rate_key = RateLimitKey::new(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id);
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn in_process_background_billing_falls_back_to_local_completion_time() {
        let state = AppState::new();
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_input_tokens(7);
        ctx.set_output_tokens(5);

        assert!(
            finalize_responses_billing_logged_with_tpm_timing(
                &state,
                &keycompute_billing::BillingService::new(),
                &ctx,
                "openai",
                uuid::Uuid::new_v4(),
                "success",
                ResponsesTpmTiming::LedgerFinishedAt,
            )
            .await
        );

        let rate_key = RateLimitKey::new(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id);
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            12
        );
    }

    #[tokio::test]
    async fn in_process_background_timeout_bills_without_current_tpm_usage() {
        let state = AppState::new();
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_input_tokens(7);
        ctx.set_output_tokens(5);

        assert!(
            finalize_responses_billing_logged_with_tpm_timing(
                &state,
                &keycompute_billing::BillingService::new(),
                &ctx,
                "openai",
                uuid::Uuid::new_v4(),
                "incomplete",
                ResponsesTpmTiming::Skip,
            )
            .await
        );

        let rate_key = RateLimitKey::new(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id);
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            0
        );
    }

    #[test]
    fn committed_ledger_tokens_override_stale_settlement_snapshot() {
        assert_eq!(
            authoritative_responses_token_counts(123, 45, 7, 5),
            (123, 45)
        );
        assert_eq!(
            authoritative_responses_token_counts(-1, -1, 7, 5),
            (7, 5),
            "corrupt out-of-range ledger values retain the defensive fallback"
        );
    }

    #[test]
    fn background_terminal_time_ignores_missing_or_invalid_provider_timestamps() {
        assert!(background_response_terminal_at(&json!({"status": "failed"})).is_none());
        assert!(
            background_response_terminal_at(&json!({
                "status": "completed",
                "completed_at": "not-a-timestamp",
            }))
            .is_none()
        );
    }

    #[test]
    fn durable_background_timeout_stays_out_of_current_tpm_on_replay() {
        let now = chrono::Utc::now();
        let mut settlement = BackgroundSettlement {
            request_id: uuid::Uuid::new_v4(),
            billing_request_id: Some(uuid::Uuid::new_v4()),
            tenant_id: uuid::Uuid::new_v4(),
            user_id: uuid::Uuid::new_v4(),
            produce_ai_key_id: uuid::Uuid::new_v4(),
            model: "gpt-test".to_string(),
            provider: "openai".to_string(),
            account_id: uuid::Uuid::new_v4(),
            pricing_snapshot: keycompute_types::PricingSnapshot::default(),
            started_at: now - chrono::Duration::hours(24),
            input_tokens: 7,
            output_tokens: 5,
            input_tokens_finalized: false,
            output_tokens_finalized: false,
            openai_beta: None,
            terminal_status: None,
            terminal_at: None,
            deadline_at: now - chrono::Duration::seconds(1),
            attempt: 1,
        };

        assert_eq!(
            background_settlement_tpm_timing(&settlement, now),
            ResponsesTpmTiming::Skip,
            "a crash after ledger commit but before persisting the timeout marker must not shift usage"
        );

        settlement.terminal_status = Some("incomplete".to_string());
        assert_eq!(
            background_settlement_tpm_timing(&settlement, now),
            ResponsesTpmTiming::Skip,
            "a rescheduled timeout marker must remain excluded from TPM"
        );

        settlement.terminal_at = Some(now);
        assert_eq!(
            background_settlement_tpm_timing(&settlement, now),
            ResponsesTpmTiming::TerminalAt(now),
            "an authoritative or locally observed terminal timestamp remains recordable"
        );
    }

    #[test]
    fn deferred_terminal_outbox_preserves_tpm_timing() {
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        let account_id = uuid::Uuid::new_v4();
        let skipped: BackgroundSettlement = serde_json::from_value(
            terminal_settlement_value_with_tpm_timing(
                &ctx,
                "openai",
                account_id,
                "incomplete",
                ResponsesTpmTiming::Skip,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(skipped.terminal_status.as_deref(), Some("incomplete"));
        assert!(skipped.terminal_at.is_none());
        assert_eq!(
            background_settlement_tpm_timing(&skipped, chrono::Utc::now()),
            ResponsesTpmTiming::Skip
        );

        let completed_at = chrono::Utc::now() - chrono::Duration::minutes(5);
        let timestamped: BackgroundSettlement = serde_json::from_value(
            terminal_settlement_value_with_tpm_timing(
                &ctx,
                "openai",
                account_id,
                "success",
                ResponsesTpmTiming::TerminalAt(completed_at),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(timestamped.terminal_at, Some(completed_at));
        assert_eq!(
            background_settlement_tpm_timing(&timestamped, chrono::Utc::now()),
            ResponsesTpmTiming::TerminalAt(completed_at)
        );
    }

    #[tokio::test]
    async fn replayed_ledger_rows_do_not_enter_the_current_tpm_window() {
        let state = AppState::new();
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        let rate_key = RateLimitKey::new(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id);

        record_responses_token_usage_values_if_fresh(
            &state,
            &ctx,
            26,
            chrono::Utc::now()
                - chrono::Duration::seconds(
                    i64::try_from(keycompute_ratelimit::WINDOW_SECS).unwrap() + 1,
                ),
        )
        .await
        .unwrap();
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            0
        );

        record_responses_token_usage_values_if_fresh(
            &state,
            &ctx,
            13,
            chrono::Utc::now()
                - chrono::Duration::milliseconds(
                    i64::try_from(keycompute_ratelimit::WINDOW_SECS * 1_000).unwrap() - 250,
                ),
        )
        .await
        .unwrap();
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            13
        );
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            0,
            "late settlement must expire at its original TPM boundary"
        );

        record_responses_token_usage_values_if_fresh(&state, &ctx, 26, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            26
        );
    }

    #[test]
    fn non_streaming_terminal_status_controls_client_trace_outcome() {
        assert_eq!(
            response_client_outcome(&json!({"status": "completed"})),
            ClientResponseOutcome::Succeeded
        );
        assert_eq!(
            response_client_outcome(&json!({"status": "queued"})),
            ClientResponseOutcome::Succeeded
        );
        assert_eq!(
            response_client_outcome(&json!({"status": "failed"})),
            ClientResponseOutcome::ResponseFailed
        );
        assert_eq!(
            response_client_outcome(&json!({"status": "incomplete"})),
            ClientResponseOutcome::ResponseFailed
        );
        assert_eq!(
            response_client_outcome(&json!({"status": "cancelled"})),
            ClientResponseOutcome::ResponseFailed
        );
        assert_eq!(
            response_billing_status(&json!({"status": "cancelled"})),
            "error"
        );
        assert_eq!(
            response_billing_status(&json!({"status": "future_status"})),
            "error"
        );
        assert_eq!(response_billing_status(&json!({})), "error");
    }

    #[test]
    fn responses_forwards_only_supported_upstream_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("openai-beta", "responses=v1".parse().unwrap());
        headers.insert("idempotency-key", "client-key".parse().unwrap());
        headers.insert("x-client-request-id", "trace/opaque.123".parse().unwrap());
        headers.insert("x-client-secret", "must-not-forward".parse().unwrap());
        let tenant = uuid::Uuid::new_v4();

        let forwarded = forwarded_responses_headers(&headers, tenant).unwrap();

        assert_eq!(
            forwarded.get("openai-beta"),
            Some(&"responses=v1".to_string())
        );
        let upstream_key = forwarded.get("idempotency-key").unwrap();
        assert_eq!(
            upstream_key,
            &upstream_responses_idempotency_key(tenant, "client-key")
        );
        assert!(!upstream_key.contains("client-key"));
        assert_eq!(
            forwarded.get("x-client-request-id"),
            Some(&"trace/opaque.123".to_string())
        );
        assert_eq!(forwarded.len(), 3);
    }

    #[test]
    fn responses_rejects_invalid_client_request_id_before_forwarding() {
        let mut headers = HeaderMap::new();
        headers.insert("x-client-request-id", "x".repeat(512).parse().unwrap());
        assert!(
            forwarded_responses_headers(&headers, uuid::Uuid::new_v4())
                .unwrap()
                .contains_key("x-client-request-id")
        );

        headers.insert("x-client-request-id", "x".repeat(513).parse().unwrap());

        assert!(matches!(
            forwarded_responses_headers(&headers, uuid::Uuid::new_v4()),
            Err(ApiError::BadRequest(message)) if message.contains("512")
        ));
    }

    #[test]
    fn responses_upstream_idempotency_is_stable_and_tenant_scoped() {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "client-key".parse().unwrap());
        let tenant = uuid::Uuid::new_v4();

        let first = forwarded_responses_headers(&headers, tenant).unwrap();
        let retry = forwarded_responses_headers(&headers, tenant).unwrap();
        let other_tenant = forwarded_responses_headers(&headers, uuid::Uuid::new_v4()).unwrap();

        assert_eq!(first["idempotency-key"], retry["idempotency-key"]);
        assert_ne!(first["idempotency-key"], other_tenant["idempotency-key"]);
        assert!(!first["idempotency-key"].contains("client-key"));
    }

    #[test]
    fn responses_idempotency_is_stable_canonical_and_tenant_scoped() {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "client-key".parse().unwrap());
        let tenant = uuid::Uuid::new_v4();
        let first = responses_idempotency(
            &headers,
            "/v1/responses",
            tenant,
            &json!({"model": "gpt-test", "input": "hello"}),
        )
        .unwrap()
        .unwrap();
        let reordered = responses_idempotency(
            &headers,
            "/v1/responses",
            tenant,
            &serde_json::from_str(r#"{"input":"hello","model":"gpt-test"}"#).unwrap(),
        )
        .unwrap()
        .unwrap();
        let other_tenant = responses_idempotency(
            &headers,
            "/v1/responses",
            uuid::Uuid::new_v4(),
            &json!({"model": "gpt-test", "input": "hello"}),
        )
        .unwrap()
        .unwrap();
        let changed = responses_idempotency(
            &headers,
            "/v1/responses",
            tenant,
            &json!({"model": "gpt-test", "input": "changed"}),
        )
        .unwrap()
        .unwrap();

        assert_eq!(first.binding_id, reordered.binding_id);
        assert_eq!(first.request_fingerprint, reordered.request_fingerprint);
        assert_eq!(first.billing_request_id, reordered.billing_request_id);
        assert_ne!(first.billing_request_id, other_tenant.billing_request_id);
        assert_ne!(first.request_fingerprint, changed.request_fingerprint);
        assert!(!first.binding_id.contains("client-key"));

        let nested_first = responses_idempotency(
            &headers,
            "/v1/responses",
            tenant,
            &serde_json::from_str(
                r#"{"input":[{"content":{"b":2,"a":1},"role":"user"}],"model":"gpt-test"}"#,
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap();
        let nested_reordered = responses_idempotency(
            &headers,
            "/v1/responses",
            tenant,
            &serde_json::from_str(
                r#"{"model":"gpt-test","input":[{"role":"user","content":{"a":1,"b":2}}]}"#,
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            nested_first.request_fingerprint,
            nested_reordered.request_fingerprint
        );
    }

    #[test]
    fn completed_idempotency_claim_replays_until_expiry() {
        let now = chrono::Utc::now();
        let mut claim = ResponsesIdempotencyClaim {
            tenant_id: uuid::Uuid::new_v4(),
            binding_id: "binding".to_string(),
            request_fingerprint: "fingerprint".to_string(),
            billing_request_id: uuid::Uuid::new_v4(),
            user_id: uuid::Uuid::new_v4(),
            produce_ai_key_id: uuid::Uuid::new_v4(),
            provider: "openai".to_string(),
            model: Some("gpt-test".to_string()),
            account_id: uuid::Uuid::new_v4(),
            execution_state: "completed".to_string(),
            execution_token: uuid::Uuid::new_v4(),
            lease_expires_at: now,
            upstream_dispatched_at: Some(now),
            response_status: Some(200),
            response_headers: Some(json!([["content-type", "application/json"]])),
            response_body: Some(r#"{"id":"resp_cached"}"#.to_string()),
            response_body_bytes: Some(i64::try_from(r#"{"id":"resp_cached"}"#.len()).unwrap()),
            response_expires_at: Some(now + chrono::Duration::minutes(1)),
            completed_at: Some(now),
            created_at: now,
        };

        let cached = cached_responses_idempotency_result(claim.clone(), None)
            .unwrap()
            .expect("completed response should replay");
        assert_eq!(cached.response.status, 200);
        assert_eq!(cached.response.body, r#"{"id":"resp_cached"}"#);
        assert!(cached.admission.is_none());

        claim.response_expires_at = Some(now - chrono::Duration::seconds(1));
        assert!(matches!(
            cached_responses_idempotency_result(claim, None),
            Err(ApiError::Conflict(message)) if message.contains("no longer replayable")
        ));
    }

    #[test]
    fn idempotency_cache_never_persists_free_form_upstream_errors() {
        let cached = cacheable_upstream_responses_error(ClientUpstreamResponse {
            status: 429,
            headers: vec![("retry-after".to_string(), "1".to_string())],
            body: json!({
                "error": {
                    "message": "secret endpoint and echoed prompt",
                    "type": "rate_limit_error",
                    "code": "rate_limit_exceeded",
                    "param": "model"
                }
            })
            .to_string(),
        })
        .unwrap();
        let body: Value = serde_json::from_str(&cached.body).unwrap();

        assert_eq!(body["error"]["message"], "Upstream request failed");
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert_eq!(body["error"]["param"], "model");
        assert!(!cached.body.contains("secret endpoint"));

        let cached = cacheable_upstream_responses_error(ClientUpstreamResponse {
            status: 429,
            headers: Vec::new(),
            body: json!({
                "error": {
                    "message": "failed",
                    "type": "credential_sk_live_secret",
                    "code": "sk_live_secret",
                    "param": "authorization_token"
                }
            })
            .to_string(),
        })
        .unwrap();
        let body: Value = serde_json::from_str(&cached.body).unwrap();

        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert!(body["error"]["param"].is_null());
        assert!(!cached.body.contains("secret"));
        assert!(!cached.body.contains("authorization_token"));
    }

    #[test]
    fn a_completed_idempotency_ledger_always_blocks_reexecution() {
        assert!(require_no_completed_responses_idempotency_ledger(false).is_ok());
        assert!(matches!(
            require_no_completed_responses_idempotency_ledger(true),
            Err(ApiError::Conflict(message)) if message.contains("no longer replayable")
        ));
    }

    #[test]
    fn settlement_permits_cap_claims_at_available_capacity() {
        let semaphore = Arc::new(Semaphore::new(3));
        let mut permits = take_available_settlement_permits(&semaphore, 16);
        assert_eq!(permits.len(), 3);
        assert!(take_available_settlement_permits(&semaphore, 16).is_empty());
        drop(permits.pop());
        assert_eq!(take_available_settlement_permits(&semaphore, 16).len(), 1);
    }

    #[test]
    fn background_settlement_round_trips_all_billing_identity() {
        let settlement = BackgroundSettlement {
            request_id: uuid::Uuid::new_v4(),
            billing_request_id: Some(uuid::Uuid::new_v4()),
            tenant_id: uuid::Uuid::new_v4(),
            user_id: uuid::Uuid::new_v4(),
            produce_ai_key_id: uuid::Uuid::new_v4(),
            model: "gpt-test".to_string(),
            provider: "openai".to_string(),
            account_id: uuid::Uuid::new_v4(),
            pricing_snapshot: keycompute_types::PricingSnapshot::default(),
            started_at: chrono::Utc::now(),
            input_tokens: 7,
            output_tokens: 9,
            input_tokens_finalized: true,
            output_tokens_finalized: false,
            openai_beta: Some("responses=v1".to_string()),
            terminal_status: None,
            terminal_at: None,
            deadline_at: chrono::Utc::now() + chrono::Duration::hours(24),
            attempt: 2,
        };
        let restored: BackgroundSettlement =
            serde_json::from_value(serde_json::to_value(&settlement).unwrap()).unwrap();
        assert_eq!(restored.request_id, settlement.request_id);
        assert_eq!(restored.billing_request_id, settlement.billing_request_id);
        assert_eq!(restored.produce_ai_key_id, settlement.produce_ai_key_id);
        assert_eq!(restored.account_id, settlement.account_id);
        assert_eq!((restored.input_tokens, restored.output_tokens), (7, 9));
        assert!(restored.input_tokens_finalized);
        assert!(!restored.output_tokens_finalized);
        assert_eq!(restored.openai_beta.as_deref(), Some("responses=v1"));
        assert_eq!(restored.attempt, 2);

        let ctx = background_billing_context(&restored, 7, 9);
        assert!(ctx.is_input_finalized());
        assert!(!ctx.is_output_finalized());
    }

    #[test]
    fn background_settlement_binds_the_account_that_produced_usage() {
        let primary_account_id = uuid::Uuid::new_v4();
        let executed_account_id = uuid::Uuid::new_v4();
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_usage_provider_account("openai-fallback", executed_account_id);

        let settlement = background_settlement_value(&ctx, "openai", primary_account_id).unwrap();
        assert_eq!(settlement["provider"], "openai-fallback");
        assert_eq!(settlement["account_id"], executed_account_id.to_string());
    }

    #[test]
    fn terminal_settlement_carries_stable_billing_identity_and_terminal_time() {
        let mut ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_billing_request_id(uuid::Uuid::new_v4());
        ctx.set_input_tokens(13);
        ctx.set_output_tokens(8);
        let settlement =
            terminal_settlement_value(&ctx, "openai", uuid::Uuid::new_v4(), "success").unwrap();

        assert_eq!(
            settlement["billing_request_id"],
            ctx.billing_request_id.to_string()
        );
        assert_eq!(settlement["terminal_status"], "success");
        assert!(settlement.get("terminal_at").is_some_and(Value::is_string));
        assert_eq!(settlement["input_tokens"], 13);
        assert_eq!(settlement["output_tokens"], 8);
        assert!(settlement_next_poll_at(&settlement) > chrono::Utc::now());
    }

    #[test]
    fn accountless_terminal_settlement_does_not_require_an_affinity_account() {
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        let value = terminal_settlement_value(
            &ctx,
            keycompute_pricing::NODE_PRICING_PROVIDER,
            uuid::Uuid::nil(),
            "success",
        )
        .unwrap();
        let settlement: BackgroundSettlement = serde_json::from_value(value).unwrap();

        assert_eq!(settlement.account_id, uuid::Uuid::nil());
        assert_eq!(settlement_affinity_account_id(&settlement), None);
        assert_eq!(settlement.terminal_status.as_deref(), Some("success"));
        assert!(matches!(
            background_settlement_tpm_timing(&settlement, chrono::Utc::now()),
            ResponsesTpmTiming::TerminalAt(_)
        ));
    }

    #[test]
    fn terminal_settlement_fallback_converges_on_billing_identity() {
        let billing_request_id = uuid::Uuid::new_v4();
        let expected_fallback = format!("resp_kc_settlement_{}", billing_request_id.simple());

        let with_resource = terminal_settlement_outbox_targets(
            Some("resp_provider_resource"),
            true,
            billing_request_id,
        );
        assert_eq!(
            with_resource,
            vec![
                ("resp_provider_resource".to_string(), true),
                (expected_fallback.clone(), false),
            ]
        );
        assert_eq!(
            terminal_settlement_outbox_targets(None, true, billing_request_id),
            vec![(expected_fallback.clone(), false)]
        );
        assert_eq!(
            terminal_settlement_outbox_targets(Some(&expected_fallback), true, billing_request_id,),
            vec![(expected_fallback, false)]
        );
    }

    #[test]
    fn background_account_snapshot_uses_the_exact_accepted_target() {
        let account_id = uuid::Uuid::new_v4();
        let ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.set_accepted_execution_target(ExecutionTarget::new_provider(
            "openai",
            account_id,
            "https://accepted.example/v1",
            "accepted-key",
        ));

        let snapshot = background_account_snapshot(&ctx, "openai", account_id).unwrap();
        assert_eq!(snapshot.account_id, account_id);
        assert_eq!(snapshot.endpoint, "https://accepted.example/v1");
        assert_eq!(snapshot.api_key, "accepted-key");
    }

    #[test]
    fn reserved_account_snapshots_replace_stale_plan_credentials() {
        let primary_id = uuid::Uuid::new_v4();
        let fallback_id = uuid::Uuid::new_v4();
        let mut plan = ExecutionPlan {
            primary: ExecutionTarget::new_provider(
                "openai",
                primary_id,
                "https://stale-primary.example/v1",
                "stale-primary-key",
            ),
            fallback_chain: vec![ExecutionTarget::new_provider(
                "openai",
                fallback_id,
                "https://stale-fallback.example/v1",
                "stale-fallback-key",
            )],
        };
        let snapshots = vec![
            ResolvedResponsesAccount {
                provider: "openai".to_string(),
                model: None,
                account_id: primary_id,
                endpoint: "https://fresh-primary.example/v1".to_string(),
                api_key: "fresh-primary-key".to_string(),
            },
            ResolvedResponsesAccount {
                provider: "openai".to_string(),
                model: None,
                account_id: fallback_id,
                endpoint: "https://fresh-fallback.example/v1".to_string(),
                api_key: "fresh-fallback-key".to_string(),
            },
        ];

        apply_reserved_account_snapshots(&mut plan, &snapshots).unwrap();

        for (target, expected_id, expected_endpoint, expected_key) in [
            (
                &plan.primary,
                primary_id,
                "https://fresh-primary.example/v1",
                "fresh-primary-key",
            ),
            (
                &plan.fallback_chain[0],
                fallback_id,
                "https://fresh-fallback.example/v1",
                "fresh-fallback-key",
            ),
        ] {
            let ExecutionTarget::ProviderAccount {
                account_id,
                endpoint,
                upstream_api_key,
                ..
            } = target
            else {
                panic!("expected provider account target");
            };
            assert_eq!(*account_id, expected_id);
            assert_eq!(endpoint, expected_endpoint);
            assert_eq!(upstream_api_key.expose(), expected_key);
        }
    }

    #[test]
    fn reclaimed_idempotency_connection_must_match_the_reserved_snapshot() {
        let account_id = uuid::Uuid::new_v4();
        let claimed = ResolvedResponsesAccount {
            provider: "openai".to_string(),
            model: Some("gpt-test".to_string()),
            account_id,
            endpoint: "https://original.example/v1".to_string(),
            api_key: "original-key".to_string(),
        };
        let matching = ExecutionPlan::new(claimed.clone().into_target());
        assert!(validate_reserved_idempotency_connection(&matching, &claimed).is_ok());

        for changed in [
            ExecutionTarget::new_provider(
                "openai",
                account_id,
                "https://replacement.example/v1",
                "original-key",
            ),
            ExecutionTarget::new_provider(
                "openai",
                account_id,
                "https://original.example/v1",
                "replacement-key",
            ),
        ] {
            assert!(matches!(
                validate_reserved_idempotency_connection(
                    &ExecutionPlan::new(changed),
                    &claimed,
                ),
                Err(ApiError::Conflict(message)) if message.contains("changed")
            ));
        }
    }

    #[test]
    fn responses_state_errors_do_not_expose_internal_details() {
        let error = responses_state_unavailable(
            "persist Responses affinity",
            "database host=db.internal password=secret",
        );
        assert!(matches!(
            error,
            ApiError::ServiceUnavailable(message)
                if message == RESPONSES_STATE_UNAVAILABLE_MESSAGE
                    && !message.contains("db.internal")
                    && !message.contains("secret")
        ));
    }

    #[test]
    fn idempotency_identity_quota_is_a_client_safe_rate_limit() {
        let error =
            map_responses_idempotency_bind_error(keycompute_db::DbError::ResourceLimitExceeded {
                resource: "Responses idempotency identities".to_string(),
                limit: "internal quota details".to_string(),
            });
        assert!(matches!(
            error,
            ApiError::RateLimit(message)
                if message.contains("Idempotency-Key quota exceeded")
                    && !message.contains("internal quota details")
        ));
    }

    #[test]
    fn background_settlement_context_drops_large_native_bodies_but_shares_usage() {
        let account_id = uuid::Uuid::new_v4();
        let mut ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            vec![Message::user("large projected payload placeholder")],
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        ctx.native_openai_responses_request = Some(Arc::new(json!({
            "input": "large payload placeholder"
        })));
        ctx.native_anthropic_request = Some(Arc::new(json!({"messages": []})));
        ctx.set_accepted_execution_target(ExecutionTarget::new_provider(
            "openai",
            account_id,
            "https://accepted.example/v1",
            "accepted-key",
        ));

        let settlement_ctx = background_settlement_context(&ctx);
        assert!(settlement_ctx.native_openai_responses_request.is_none());
        assert!(settlement_ctx.native_anthropic_request.is_none());
        assert!(settlement_ctx.messages.is_empty());
        assert_eq!(
            background_account_snapshot(&settlement_ctx, "openai", account_id)
                .unwrap()
                .api_key,
            "accepted-key"
        );

        ctx.set_input_tokens(17);
        ctx.set_output_tokens(11);
        assert_eq!(settlement_ctx.usage_snapshot(), (17, 11));
    }
}
