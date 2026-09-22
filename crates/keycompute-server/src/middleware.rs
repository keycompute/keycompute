//! 中间件
//!
//! 自定义中间件：认证、限流、可观测性等

use crate::{
    error::{ApiError, Result, TrustedLocalApiError},
    extractors::{AuthExtractor, ClientRequestId, RequestId, RequestReceivedAt},
    handlers::responses::{
        OPENAI_RESPONSES_BODY_LIMIT_BYTES, OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES,
    },
    handlers::{
        anthropic::{
            ANTHROPIC_MESSAGES_BODY_LIMIT_BYTES, ANTHROPIC_MESSAGES_REQUEST_WORKING_SET_LIMIT_BYTES,
        },
        openai::{OPENAI_CHAT_BODY_LIMIT_BYTES, OPENAI_CHAT_REQUEST_WORKING_SET_LIMIT_BYTES},
    },
    state::{AppState, GENERATION_LARGE_HTTP_BODY_BYTES},
};
use axum::{
    body::{Body, to_bytes},
    extract::{FromRequestParts, Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE, SET_COOKIE},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use keycompute_auth::Permission;
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey};
use llm_protocol_provider::{
    LARGE_JSON_WORKING_SET_ADMISSION_BYTES, estimated_json_parse_working_set_bytes,
};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use uuid::Uuid;

const PUBLIC_AUTH_COOKIE_NAME: &str = "keyc_reg_sid";
const PUBLIC_AUTH_COOKIE_MAX_AGE_SECS: i64 = 60 * 60 * 24 * 30;
/// Maximum wall-clock time spent receiving a generation JSON request body.
///
/// The largest supported inline skill payload is roughly 70 MiB, so this is
/// deliberately longer than the normal upstream request timeout while still
/// preventing an authenticated slow client from holding a process-wide body
/// admission permit indefinitely.
const GENERATION_HTTP_BODY_READ_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, PartialEq, Eq)]
enum GenerationHttpBodyReadError {
    InvalidOrTooLarge,
    MemoryCapacity,
    Timeout,
}

#[cfg(test)]
async fn read_generation_http_body(
    body: Body,
    limit: usize,
    timeout: Duration,
) -> std::result::Result<bytes::Bytes, GenerationHttpBodyReadError> {
    read_budgeted_generation_http_body(body, limit, timeout)
        .await
        .map(|(body, _)| body)
}

struct BudgetedInputBytes {
    bytes: bytes::Bytes,
    _memory: keycompute_types::memory::MemoryPermit,
}
impl AsRef<[u8]> for BudgetedInputBytes {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}
async fn read_budgeted_generation_http_body(
    body: Body,
    limit: usize,
    timeout: Duration,
) -> std::result::Result<
    (bytes::Bytes, keycompute_types::memory::MemoryPermit),
    GenerationHttpBodyReadError,
> {
    use futures::StreamExt;
    let memory = keycompute_types::memory::reserve_process_memory(0)
        .map_err(|_| GenerationHttpBodyReadError::MemoryCapacity)?;
    let operation = async {
        let mut chunks = body.into_data_stream();
        let mut retained = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| GenerationHttpBodyReadError::InvalidOrTooLarge)?;
            let next = retained
                .len()
                .checked_add(chunk.len())
                .filter(|n| *n <= limit)
                .ok_or(GenerationHttpBodyReadError::InvalidOrTooLarge)?;
            // Vec replacement + the incoming frame may coexist briefly.
            memory
                .grow_to(next.saturating_mul(3))
                .map_err(|_| GenerationHttpBodyReadError::MemoryCapacity)?;
            retained
                .try_reserve(chunk.len())
                .map_err(|_| GenerationHttpBodyReadError::MemoryCapacity)?;
            retained.extend_from_slice(&chunk);
        }
        retained.shrink_to_fit();
        let bytes = bytes::Bytes::from_owner(BudgetedInputBytes {
            bytes: bytes::Bytes::from(retained),
            _memory: memory.clone(),
        });
        Ok((bytes, memory))
    };
    tokio::time::timeout(timeout, operation)
        .await
        .map_err(|_| GenerationHttpBodyReadError::Timeout)?
}

/// Marks a maintenance response whose administrator-configured message is
/// explicitly public. Responses error normalization may preserve this message,
/// while continuing to redact arbitrary service-unavailable details.
#[derive(Clone, Copy, Debug)]
struct TrustedPublicMaintenanceError;

/// Admit potentially large generation bodies before Axum's JSON extractor
/// buffers them. Requests without Content-Length are treated conservatively as
/// large while they are being received; after buffering, the permit is kept
/// only when the actual body or its estimated parse working set is large. The
/// normal body limit remains the final per-request bound.
pub async fn generation_http_body_admission_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(policy) = generation_json_body_policy(req.method(), req.uri().path()) else {
        return next.run(req).await;
    };

    // Authenticate before reading any body bytes. The rate-limit middleware
    // deliberately lets invalid credentials reach the normal auth path, so
    // this middleware must not rely on rate limiting as an auth boundary.
    let (mut parts, body) = req.into_parts();
    let mut auth = match AuthExtractor::from_request_parts(&mut parts, &state).await {
        Ok(auth) => auth,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = crate::admission::ensure_generation(&state, &mut auth).await {
        let mut response = error.into_response();
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        return response;
    }
    let generation_permit = auth.generation_permit.clone();
    parts.extensions.insert(auth);

    let content_length = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if content_length.is_some_and(|bytes| bytes > policy.body_limit_bytes as u64) {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }

    let mut permit = if content_length.is_none_or(|bytes| bytes > GENERATION_LARGE_HTTP_BODY_BYTES)
    {
        let Some(permit) = state.generation_http_body_admission.try_acquire() else {
            return ApiError::RateLimit(format!(
                "Too many large {} request bodies are active",
                policy.name
            ))
            .into_response();
        };
        Some(permit)
    } else {
        None
    };

    // Inspect the serialized representation before Axum's JSON extractor
    // builds a potentially much larger Value tree. Rebuild the body from
    // Bytes so the extractor retains its normal content-type/error behavior.
    let (body, memory) = match read_budgeted_generation_http_body(
        body,
        policy.body_limit_bytes,
        GENERATION_HTTP_BODY_READ_TIMEOUT,
    )
    .await
    {
        Ok(body) => body,
        Err(GenerationHttpBodyReadError::InvalidOrTooLarge) => {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        Err(GenerationHttpBodyReadError::Timeout) => {
            return StatusCode::REQUEST_TIMEOUT.into_response();
        }
        Err(GenerationHttpBodyReadError::MemoryCapacity) => {
            return ApiError::ServiceUnavailable(
                "Process payload memory capacity exhausted".into(),
            )
            .into_response();
        }
    };
    // Retain room for JSON text/tree and the native/context projection copies.
    if memory
        .grow_to(estimated_json_parse_working_set_bytes(&body).saturating_mul(2))
        .is_err()
    {
        return ApiError::ServiceUnavailable("Process payload memory capacity exhausted".into())
            .into_response();
    }
    let needs_working_set_admission = match generation_json_body_needs_working_set_admission(
        &body,
        policy.working_set_limit_bytes,
        LARGE_JSON_WORKING_SET_ADMISSION_BYTES,
    ) {
        Ok(needs_admission) => needs_admission,
        Err(()) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let needs_resident_body_admission =
        body.len() as u64 > GENERATION_LARGE_HTTP_BODY_BYTES || needs_working_set_admission;
    if permit.is_some() && !needs_resident_body_admission {
        // Content-Length is optional (for example with chunked transfer
        // encoding), so an unknown-size body needs a provisional receive
        // permit. Do not retain that scarce permit through inference once the
        // fully-buffered representation proves small.
        permit = None;
    } else if permit.is_none() && needs_resident_body_admission {
        let Some(acquired) = state.generation_http_body_admission.try_acquire() else {
            return ApiError::RateLimit(format!(
                "Too many large {} request bodies are active",
                policy.name
            ))
            .into_response();
        };
        permit = Some(acquired);
    }

    let memory_lifetime = memory.clone();
    let permit = match permit {
        Some(permit) => permit.with_memory(memory),
        None => crate::state::GenerationHttpBodyPermit::memory_only(memory),
    };
    let mut req = Request::from_parts(parts, Body::from(body));
    req.extensions_mut().insert(permit);
    let response = crate::admission::retain_response(next.run(req).await, generation_permit);
    crate::admission::retain_memory(response, memory_lifetime)
}

#[derive(Clone, Copy)]
struct GenerationJsonBodyPolicy {
    name: &'static str,
    body_limit_bytes: usize,
    working_set_limit_bytes: usize,
}

fn generation_json_body_policy(method: &Method, path: &str) -> Option<GenerationJsonBodyPolicy> {
    if method != Method::POST {
        return None;
    }
    match path {
        p if keycompute_types::ModelAccessMode::from_chat_path(p).is_some() => {
            Some(GenerationJsonBodyPolicy {
                name: "Chat Completions",
                body_limit_bytes: OPENAI_CHAT_BODY_LIMIT_BYTES,
                working_set_limit_bytes: OPENAI_CHAT_REQUEST_WORKING_SET_LIMIT_BYTES,
            })
        }
        "/v1/messages" | "/pt/v1/messages" | "/nt/v1/messages" => Some(GenerationJsonBodyPolicy {
            name: "Anthropic Messages",
            body_limit_bytes: ANTHROPIC_MESSAGES_BODY_LIMIT_BYTES,
            working_set_limit_bytes: ANTHROPIC_MESSAGES_REQUEST_WORKING_SET_LIMIT_BYTES,
        }),
        "/v1/responses"
        | "/pt/v1/responses"
        | "/nt/v1/responses"
        | "/v1/responses/compact"
        | "/v1/responses/input_tokens" => Some(GenerationJsonBodyPolicy {
            name: "Responses",
            body_limit_bytes: OPENAI_RESPONSES_BODY_LIMIT_BYTES,
            working_set_limit_bytes: OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES,
        }),
        _ => None,
    }
}

fn generation_json_body_needs_working_set_admission(
    body: &[u8],
    max_working_set_bytes: usize,
    admission_threshold_bytes: usize,
) -> std::result::Result<bool, ()> {
    let working_set_bytes = estimated_json_parse_working_set_bytes(body);
    if working_set_bytes > max_working_set_bytes {
        return Err(());
    }
    Ok(working_set_bytes > admission_threshold_bytes)
}

#[cfg(test)]
fn generation_request_needs_body_admission(
    method: &Method,
    path: &str,
    content_length: Option<u64>,
) -> bool {
    generation_json_body_policy(method, path).is_some()
        && content_length.is_none_or(|bytes| bytes > GENERATION_LARGE_HTTP_BODY_BYTES)
}

/// 权限中间件的返回类型
pub type PermissionMiddlewareFn =
    fn(
        State<AppState>,
        AuthExtractor,
        Request,
        Next,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response>> + Send>>;

/// 纯文本 400/415 错误体首行回显给客户端的最大长度。
///
/// axum `Json` 提取器的反序列化提示是字段级排障信息（不含服务端内部细节），
/// 但异常长的纯文本体不应被完整回显；超长部分截断即可。
const MAX_PLAINTEXT_ERROR_CHARS: usize = 256;

/// 错误响应体读取超时。
///
/// 非 2xx 响应在本服务中都是即时的小 body，但防御性加上限时：若未来某个
/// 中间件返回流式（chunked）错误体，`to_bytes` 会挂起直到流结束；超时后
/// 回退通用文本，保证 /v1/messages 的错误路径不会因此挂起请求。
const ERROR_BODY_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// 将 `/v1/messages` 的 HTTP 失败响应转换为 Anthropic Errors schema。
///
/// 流建立后的错误由 handler 以 SSE `error` 事件输出；这里处理鉴权、JSON
/// 反序列化、限流和 handler 返回的非 2xx，使 SDK 在所有 HTTP 错误路径都能
/// 使用同一结构解析。
pub async fn anthropic_error_response_middleware(req: Request, next: Next) -> Response {
    let is_anthropic_messages = req.uri().path() == "/v1/messages";
    let response = next.run(req).await;
    // 成功与重定向原样透传：3xx 携带 Location 等跳转语义，改写成错误 JSON
    // 会破坏重定向流程。
    if !is_anthropic_messages
        || response.status().is_success()
        || response.status().is_redirection()
    {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let body = match tokio::time::timeout(ERROR_BODY_READ_TIMEOUT, to_bytes(body, 64 * 1024)).await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) | Err(_) => bytes::Bytes::new(),
    };
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);

    // 避免重复封装已经符合 Anthropic schema 的响应。
    if parsed.get("type").and_then(serde_json::Value::as_str) == Some("error")
        && parsed
            .get("error")
            .is_some_and(serde_json::Value::is_object)
    {
        return Response::from_parts(parts, Body::from(body));
    }

    let message = parsed
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            parsed
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        // axum 的 `Json` 提取器失败时返回纯文本 400/415（如缺失 `max_tokens`、
        // 字段类型错误）。这类提示只反映客户端请求自身的问题，不涉及服务端
        // 内部细节，提取首行可安全返回，便于 SDK 客户端排障；其它非 JSON
        // 错误体（如代理错误页）保持通用文本。首行同时被截断，防止异常长的
        // 纯文本体被完整回显到响应。
        .or_else(|| {
            if !matches!(parts.status.as_u16(), 400 | 415) {
                return None;
            }
            std::str::from_utf8(&body)
                .ok()?
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| {
                    line.chars()
                        .take(MAX_PLAINTEXT_ERROR_CHARS)
                        .collect::<String>()
                })
        })
        .unwrap_or_else(|| "Request failed".to_string());
    let error_type = anthropic_error_type(parts.status);

    parts.headers.remove(CONTENT_LENGTH);
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let body = serde_json::json!({
        "type": "error",
        "error": {"type": error_type, "message": message}
    })
    .to_string();
    Response::from_parts(parts, Body::from(body))
}

/// Normalize failures on every Responses HTTP resource to the OpenAI error
/// envelope. Upstream type/code/param fields remain useful to SDKs, but their
/// free-form message is untrusted and must not cross the public boundary.
pub async fn openai_responses_error_response_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let is_responses = path == "/v1/responses" || path.starts_with("/v1/responses/");
    let response = next.run(req).await;
    if !is_responses
        || response.status().is_informational()
        || response.status().is_success()
        || response.status().is_redirection()
    {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let body =
        match tokio::time::timeout(ERROR_BODY_READ_TIMEOUT, to_bytes(body, 1024 * 1024)).await {
            Ok(Ok(body)) => body,
            Ok(Err(_)) | Err(_) => bytes::Bytes::new(),
        };
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let error = parsed.get("error").and_then(serde_json::Value::as_object);

    // ApiError responses and the explicitly public maintenance response carry
    // out-of-band markers for trustworthy client-facing messages. JSON shape
    // alone is insufficient: a compatible upstream can imitate either shape.
    let is_trusted_local_error = parts.extensions.get::<TrustedLocalApiError>().is_some();
    let is_public_maintenance_error = parts
        .extensions
        .get::<TrustedPublicMaintenanceError>()
        .is_some();
    if !is_trusted_local_error && !is_public_maintenance_error {
        parts.headers.remove(CONTENT_LENGTH);
        parts
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let body = normalize_openai_responses_upstream_error(parts.status, &body);
        return Response::from_parts(parts, Body::from(body));
    }
    let official_error = error.filter(|error| {
        error
            .get("message")
            .is_some_and(serde_json::Value::is_string)
            && error.get("type").is_some_and(serde_json::Value::is_string)
            && error
                .get("code")
                .is_some_and(|code| code.is_string() || code.is_null())
    });
    let trusted_local_error_shape = error.is_some_and(|error| {
        let numeric_status_code = error.get("code").and_then(serde_json::Value::as_u64)
            == Some(u64::from(parts.status.as_u16()));
        let local_rate_limit = parts.status == StatusCode::TOO_MANY_REQUESTS
            && matches!(
                error.get("type").and_then(serde_json::Value::as_str),
                Some("rate_limit_error" | "rate_limit_exceeded")
            )
            && error.get("code").and_then(serde_json::Value::as_str) == Some("rate_limit_exceeded");
        numeric_status_code || local_rate_limit
    });
    let message = error
        .filter(|error| {
            (is_trusted_local_error && parts.status.is_client_error() && trusted_local_error_shape)
                || (is_public_maintenance_error
                    && parts.status == StatusCode::SERVICE_UNAVAILABLE
                    && error.get("type").and_then(serde_json::Value::as_str)
                        == Some("maintenance_mode")
                    && error.get("code").and_then(serde_json::Value::as_str)
                        == Some("service_unavailable"))
        })
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if official_error.is_some() {
                "Upstream request failed".to_string()
            } else {
                "Request failed".to_string()
            }
        });
    let (error_type, param, code) =
        normalized_openai_responses_error_fields(official_error, parts.status);

    parts.headers.remove(CONTENT_LENGTH);
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": param,
            "code": code,
        }
    })
    .to_string();
    Response::from_parts(parts, Body::from(body))
}

/// Normalize only trusted, locally-produced 429 responses on the OpenAI Chat
/// and Models routes. Generic `/api/v1/**` clients retain the service's normal
/// REST error contract, while native upstream 429 responses remain untouched.
pub async fn openai_rate_limit_response_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let is_openai_route = path.starts_with("/nt/v1/")
        || path == "/v1/chat/completions"
        || path == "/pt/v1/chat/completions"
        || path == "/v1/models"
        || path.starts_with("/v1/models/")
        || path == "/pt/v1/models"
        || path.starts_with("/pt/v1/models/");
    let response = next.run(req).await;
    if !is_openai_route
        || response.status() != StatusCode::TOO_MANY_REQUESTS
        || response
            .extensions()
            .get::<TrustedLocalApiError>()
            .is_none()
    {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let body = to_bytes(body, 64 * 1024).await.unwrap_or_default();
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let message = parsed
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Rate limit exceeded. Please try again later.");
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "rate_limit_error",
            "param": serde_json::Value::Null,
            "code": "rate_limit_exceeded",
        }
    })
    .to_string();
    parts.headers.remove(CONTENT_LENGTH);
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Response::from_parts(parts, Body::from(body))
}

/// Produce the stable, client-visible form of an untrusted Responses error.
/// This is also stored by the idempotency cache, so raw provider messages do
/// not become durable data and a replay matches the middleware's first pass.
pub(crate) fn normalize_openai_responses_upstream_error(status: StatusCode, body: &[u8]) -> String {
    let parsed: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    let error = parsed.get("error").and_then(serde_json::Value::as_object);
    let official_error = error.filter(|error| {
        error
            .get("message")
            .is_some_and(serde_json::Value::is_string)
            && error.get("type").is_some_and(serde_json::Value::is_string)
            && error
                .get("code")
                .is_some_and(|code| code.is_string() || code.is_null())
    });
    let message = if official_error.is_some() {
        "Upstream request failed"
    } else {
        "Request failed"
    };
    let (error_type, param, code) =
        normalized_openai_responses_error_fields(official_error, status);
    serde_json::json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": param,
            "code": code,
        }
    })
    .to_string()
}

pub(crate) fn openai_responses_error_fields(
    status: StatusCode,
) -> (&'static str, serde_json::Value) {
    match status.as_u16() {
        401 => ("authentication_error", serde_json::json!("invalid_api_key")),
        403 => ("permission_error", serde_json::Value::Null),
        413 => (
            "invalid_request_error",
            serde_json::json!("request_too_large"),
        ),
        429 => ("rate_limit_error", serde_json::json!("rate_limit_exceeded")),
        500..=599 => ("server_error", serde_json::json!("server_error")),
        _ => ("invalid_request_error", serde_json::Value::Null),
    }
}

fn normalized_openai_responses_error_fields(
    error: Option<&serde_json::Map<String, serde_json::Value>>,
    status: StatusCode,
) -> (serde_json::Value, serde_json::Value, serde_json::Value) {
    let (default_error_type, default_code) = openai_responses_error_fields(status);
    (
        sanitize_openai_responses_error_type(
            error.and_then(|error| error.get("type")),
            default_error_type,
        ),
        sanitize_openai_responses_error_param(error.and_then(|error| error.get("param"))),
        sanitize_openai_responses_error_code(
            error.and_then(|error| error.get("code")),
            default_code,
        ),
    )
}

/// Keep only stable, explicitly supported error classifications. These fields
/// are untrusted provider data just like `message`; accepting arbitrary strings
/// would give credentials or provider internals another path to the client and
/// the durable idempotency cache.
pub(crate) fn sanitize_openai_responses_error_type(
    value: Option<&serde_json::Value>,
    fallback: &str,
) -> serde_json::Value {
    const ALLOWED: &[&str] = &[
        "authentication_error",
        "conflict_error",
        "invalid_request_error",
        "maintenance_mode",
        "not_found_error",
        "permission_error",
        "rate_limit_error",
        "server_error",
        "unprocessable_entity_error",
    ];
    value
        .and_then(serde_json::Value::as_str)
        .filter(|value| ALLOWED.contains(value))
        .unwrap_or(fallback)
        .to_string()
        .into()
}

pub(crate) fn sanitize_openai_responses_error_code(
    value: Option<&serde_json::Value>,
    fallback: serde_json::Value,
) -> serde_json::Value {
    const ALLOWED: &[&str] = &[
        "execution_authority_invalid",
        "execution_authority_unavailable",
        "context_length_exceeded",
        "file_not_found",
        "insufficient_quota",
        "invalid_api_key",
        "invalid_base64_image",
        "invalid_file",
        "invalid_file_format",
        "invalid_file_purpose",
        "invalid_file_size",
        "invalid_file_type",
        "invalid_image",
        "invalid_image_format",
        "invalid_image_mode",
        "invalid_image_url",
        "invalid_prompt",
        "invalid_request_error",
        "invalid_tool",
        "invalid_value",
        "missing_required_parameter",
        "model_not_found",
        "provider_failure",
        "rate_limit_exceeded",
        "request_too_large",
        "server_error",
        "service_unavailable",
        "unknown_parameter",
        "unsupported_image_media_type",
        "unsupported_parameter",
        "unsupported_value",
    ];
    match value {
        Some(serde_json::Value::Null) => serde_json::Value::Null,
        Some(serde_json::Value::String(value)) if ALLOWED.contains(&value.as_str()) => {
            serde_json::Value::String(value.clone())
        }
        _ => fallback,
    }
}

pub(crate) fn sanitize_openai_responses_error_param(
    value: Option<&serde_json::Value>,
) -> serde_json::Value {
    const ALLOWED: &[&str] = &[
        "audio",
        "background",
        "conversation",
        "frequency_penalty",
        "include",
        "input",
        "instructions",
        "logit_bias",
        "logprobs",
        "max_completion_tokens",
        "max_output_tokens",
        "max_tokens",
        "max_tool_calls",
        "messages",
        "metadata",
        "model",
        "modalities",
        "n",
        "parallel_tool_calls",
        "prediction",
        "presence_penalty",
        "previous_response_id",
        "prompt",
        "prompt_cache_key",
        "prompt_cache_options",
        "prompt_cache_retention",
        "reasoning",
        "reasoning_effort",
        "response_format",
        "safety_identifier",
        "seed",
        "service_tier",
        "stop",
        "store",
        "stream",
        "stream_options",
        "temperature",
        "text",
        "tool_choice",
        "tools",
        "top_logprobs",
        "top_p",
        "truncation",
        "user",
        "verbosity",
        "web_search_options",
    ];
    match value {
        Some(serde_json::Value::String(value)) if ALLOWED.contains(&value.as_str()) => {
            serde_json::Value::String(value.clone())
        }
        _ => serde_json::Value::Null,
    }
}

/// 将本服务的 HTTP 状态映射至 Anthropic 的公开错误类别。保留原始 HTTP
/// 状态码，只统一 SDK 读取的 `error.type`。
fn anthropic_error_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 405 | 415 | 422 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        // Anthropic 的标准过载状态是 529；本服务既有的 503 也表示暂时
        // 无可用容量，向客户端暴露相同可重试类别更准确。
        503 | 529 => "overloaded_error",
        _ => "api_error",
    }
}

/// Log a resource path without invitation/reset capabilities or query values.
/// The same projection is used by request logs, spans and maintenance logs.
pub(crate) fn request_log_path(uri: &axum::http::Uri) -> &str {
    let path = uri.path();
    if path.starts_with("/api/v1/invitations/") {
        "/api/v1/invitations/[redacted]/accept"
    } else if path.starts_with("/api/v1/auth/verify-reset-token/") {
        "/api/v1/auth/verify-reset-token/[redacted]"
    } else {
        path
    }
}

/// 请求日志中间件
pub async fn request_logger(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let uri = request_log_path(req.uri()).to_owned();

    // 提前克隆 request_id，避免借用冲突
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    info!(
        request_id = %request_id,
        method = %method,
        uri = %uri,
        "Request started"
    );

    let response = next.run(req).await;

    let duration = start.elapsed();
    let status = response.status();

    info!(
        request_id = %request_id,
        method = %method,
        uri = %uri,
        status = %status.as_u16(),
        duration_ms = %duration.as_millis(),
        "Request completed"
    );

    response
}

/// CORS 中间件配置
pub fn cors_layer() -> tower_http::cors::CorsLayer {
    tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
        .expose_headers([
            HeaderName::from_static("x-request-id"),
            HeaderName::from_static("x-client-request-id"),
            HeaderName::from_static("retry-after"),
            HeaderName::from_static("x-ratelimit-scope"),
        ])
}

/// 请求身份与入口时间注入中间件
pub async fn trace_id_middleware(
    State(_state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let received_at = chrono::Utc::now();
    let request_id = Uuid::new_v4();
    let client_request_id = req
        .headers()
        .get("X-Request-ID")
        .and_then(|value| value.to_str().ok())
        .and_then(validate_client_request_id);
    if req.headers().contains_key("X-Request-ID") && client_request_id.is_none() {
        keycompute_observability::metrics::CLIENT_REQUEST_ID_REJECTED_TOTAL.inc();
        tracing::debug!("Rejected invalid client request ID");
    }
    req.extensions_mut().insert(RequestId(request_id));
    req.extensions_mut().insert(RequestReceivedAt(received_at));
    req.extensions_mut()
        .insert(ClientRequestId(client_request_id.clone()));
    let mut response = next.run(req).await;
    let internal_value =
        HeaderValue::from_str(&request_id.to_string()).expect("UUID is a valid header");
    response
        .headers_mut()
        .insert("X-Request-ID", internal_value);
    if let Some(client_request_id) =
        client_request_id.and_then(|value| HeaderValue::from_str(&value).ok())
    {
        response
            .headers_mut()
            .insert("X-Client-Request-ID", client_request_id);
    }
    response
}

/// Validate the optional, untrusted client correlation identifier.
pub fn validate_client_request_id(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(byte))
    {
        return None;
    }
    Some(value.to_string())
}

/// 构造服务不可用响应（503 Service Unavailable）
///
/// 用于限流检查出错时（如 Redis 不可用），遵循 fail-closed 安全原则。
fn service_unavailable_response() -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({
            "error": {
                "message": "Rate limit check failed. Please try again later.",
                "type": "service_unavailable",
                "code": "rate_limit_check_failed"
            }
        })),
    )
        .into_response();
    response.extensions_mut().insert(TrustedLocalApiError);
    response
}

/// Construct a sanitized 503 for authentication dependency failures.
fn authentication_service_unavailable_response() -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({
            "error": {
                "message": "Authentication service is temporarily unavailable. Please try again later.",
                "type": "service_unavailable",
                "code": "authentication_unavailable"
            }
        })),
    )
        .into_response();
    response.extensions_mut().insert(TrustedLocalApiError);
    response
}

/// Load the authenticated tenant's limits from the authoritative database.
/// A configured database is part of the quota decision, so a missing tenant
/// or a failed lookup must not silently widen the request to global defaults.
pub(crate) async fn authenticated_rate_limit_config(
    state: &AppState,
    tenant_id: Uuid,
) -> Result<RateLimitConfig> {
    let Some(pool) = state.pool.as_deref() else {
        return Ok(RateLimitConfig::default());
    };
    match keycompute_db::Tenant::find_by_id(pool.write_conn(), tenant_id).await {
        Ok(Some(tenant)) => Ok(RateLimitConfig::from_tenant(
            tenant.default_rpm_limit,
            tenant.default_tpm_limit,
        )),
        Ok(None) => {
            error!(
                %tenant_id,
                "Authenticated tenant not found for rate limiting, denying request"
            );
            Err(ApiError::ServiceUnavailable(
                "Rate limit configuration is unavailable. Please try again later.".to_string(),
            ))
        }
        Err(error) => {
            error!(
                %tenant_id,
                %error,
                "Failed to load authenticated tenant for rate limiting, denying request"
            );
            Err(ApiError::ServiceUnavailable(
                "Rate limit configuration is unavailable. Please try again later.".to_string(),
            ))
        }
    }
}

/// Load the effective generation limits for a tenant and its selected account.
///
/// The tenant limit is always authoritative. An account may further tighten
/// that limit, but it must never widen it. In a configured deployment, an
/// account lookup failure is itself a rate-limit dependency failure and is
/// therefore returned to the caller instead of silently falling back to the
/// global defaults.
pub(crate) async fn authenticated_rate_limit_config_for_account(
    state: &AppState,
    tenant_id: Uuid,
    account_id: Option<Uuid>,
) -> Result<RateLimitConfig> {
    authenticated_rate_limit_config_for_selection(state, tenant_id, account_id, None).await
}

/// The trusted execution target carries passthrough provenance. An explicit
/// cross-owner grant must not be rejected using obsolete account visibility.
pub(crate) async fn authenticated_rate_limit_config_for_target(
    state: &AppState,
    tenant_id: Uuid,
    target: &keycompute_types::ExecutionTarget,
) -> Result<RateLimitConfig> {
    authenticated_rate_limit_config_for_selection(
        state,
        tenant_id,
        target.account_id(),
        target.account_selection(),
    )
    .await
}

async fn authenticated_rate_limit_config_for_selection(
    state: &AppState,
    tenant_id: Uuid,
    account_id: Option<Uuid>,
    selection: Option<keycompute_types::AccountSelection>,
) -> Result<RateLimitConfig> {
    let tenant_config = authenticated_rate_limit_config(state, tenant_id).await?;

    let Some(account_id) = account_id else {
        return Ok(tenant_config);
    };
    let Some(pool) = state.pool.as_deref() else {
        // In-memory/test deployments have no authoritative account table.
        return Ok(tenant_config);
    };

    let account = match keycompute_db::Account::find_by_id(pool.write_conn(), account_id).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            error!(
                %account_id,
                "Selected account not found for rate limiting, denying request"
            );
            return Err(ApiError::ServiceUnavailable(
                "Rate limit configuration is unavailable. Please try again later.".to_string(),
            ));
        }
        Err(error) => {
            error!(
                %account_id,
                %error,
                "Failed to load selected account for rate limiting, denying request"
            );
            return Err(ApiError::ServiceUnavailable(
                "Rate limit configuration is unavailable. Please try again later.".to_string(),
            ));
        }
    };

    let authorized = if let Some(keycompute_types::AccountSelection::PassthroughBinding {
        binding_id,
        binding_revision,
    }) = selection
    {
        use sea_orm::ConnectionTrait;
        tokio::time::timeout(std::time::Duration::from_secs(3), pool.write_conn().query_one(
            sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres,
            "SELECT 1 FROM passthrough_bindings b JOIN accounts a ON a.id=b.account_id JOIN tenants anchor ON anchor.id=b.tenant_id AND anchor.status='active' JOIN tenants owner ON owner.id=a.tenant_id AND owner.status='active' WHERE b.id=$1 AND b.revision=$2 AND b.account_id=$3 AND (b.tenant_id=$4 OR b.is_global) AND a.enabled AND EXISTS(SELECT 1 FROM tenants caller WHERE caller.id=$4 AND caller.status='active')",
            [binding_id.into(), binding_revision.into(), account_id.into(), tenant_id.into()])))
            .await.map_err(|_|ApiError::ServiceUnavailable("Rate limit access lookup timed out".into()))?
            .map_err(|_|ApiError::ServiceUnavailable("Rate limit access state unavailable".into()))?.is_some()
    } else {
        keycompute_db::Account::authorize_non_pt(pool.write_conn(), tenant_id, account_id)
            .await
            .map_err(|_| {
                ApiError::ServiceUnavailable("Rate limit access state unavailable".into())
            })?
    };
    if !authorized {
        return Err(ApiError::ServiceUnavailable(
            "Selected account access changed before generation admission".into(),
        ));
    }
    if !account.enabled {
        error!(
            %account_id,
            "Selected account was disabled before generation admission, denying request"
        );
        return Err(ApiError::ServiceUnavailable(
            "Rate limit configuration is unavailable. Please try again later.".to_string(),
        ));
    }

    Ok(stricter_rate_limit_config(
        tenant_config,
        RateLimitConfig::from_tenant(account.rpm_limit, account.tpm_limit),
    ))
}

fn stricter_rate_limit_config(
    tenant_config: RateLimitConfig,
    account_config: RateLimitConfig,
) -> RateLimitConfig {
    RateLimitConfig::new(
        tenant_config.rpm_limit.min(account_config.rpm_limit),
        tenant_config.tpm_limit.min(account_config.tpm_limit),
    )
}

/// 限流中间件
///
/// 基于用户/租户/API Key 进行请求限流
/// 支持从数据库加载租户特定的 RPM/TPM 配置
/// 注意：此中间件应在认证中间件之后运行，以获取真实的认证信息
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    // Console traffic has an independent class/user/tenant/aggregate budget.
    // Do not charge it to generation RPM/TPM or allow a second limiter pass.
    if req
        .extensions()
        .get::<crate::console::ConsoleAdmissionChecked>()
        .is_some()
        && keycompute_types::console::classify(req.method().as_str(), req.uri().path()).is_some()
    {
        return next.run(req).await;
    }
    // WebSocket 握手只建立传输连接，不等同于一次 Responses 生成请求。
    // 每个 `response.create` 会在 WebSocket handler 内独立执行 RPM/TPM
    // 检查；这里跳过握手，避免首个事件被重复计数。
    if req.method() == axum::http::Method::GET
        && req.uri().path() == "/v1/responses"
        && req
            .headers()
            .get(axum::http::header::UPGRADE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
    {
        return next.run(req).await;
    }
    // Reuse authentication performed by an outer middleware/extractor. Apart
    // from avoiding a redundant database read, this is important for fail
    // closed behavior: a transient failure during a second verification must
    // not turn an already-authenticated admin request into an RPM bypass.
    let auth = if let Some(auth) = req.extensions().get::<AuthExtractor>().cloned() {
        auth
    } else {
        // No cached identity is available, so parse credentials exactly as the
        // normal authentication extractor does. Credential errors continue to
        // the authentication layer so it can return its canonical 401.
        let headers = req.headers();
        let token = match headers.get("Authorization").and_then(|h| h.to_str().ok()) {
            Some(auth_header) => match auth_header.strip_prefix("Bearer ") {
                Some(token) => token,
                None => return next.run(req).await,
            },
            None => {
                // 与认证提取器保持一致：x-api-key 是 Anthropic 传输层约定，
                // 仅 /v1/messages 使用；其他路径不得以 x-api-key 身份消耗配额。
                if !x_api_key_allowed_on_path(req.uri().path()) {
                    return next.run(req).await;
                }
                match headers.get("x-api-key").and_then(|h| h.to_str().ok()) {
                    Some(token) if !token.is_empty() => token,
                    _ => return next.run(req).await,
                }
            }
        };

        match state.auth.verify_token(token).await {
            Ok(auth_context) => match AuthExtractor::from_auth_context(auth_context) {
                Ok(a) => a,
                Err(_) => return next.run(req).await,
            },
            Err(keycompute_types::KeyComputeError::AuthError(_)) => {
                return next.run(req).await;
            }
            Err(error) => {
                error!(%error, "Authentication backend failed during rate limiting, denying request");
                return authentication_service_unavailable_response();
            }
        }
    };

    if req.method() == Method::POST
        && keycompute_types::ModelAccessMode::uses_execution_rpm(req.uri().path())
    {
        // Generation RPM is execution-based: route-aware handlers own the
        // admission because the effective upstream account is only known after
        // routing and project-level affinity checks. Malformed requests,
        // routing failures, and completed idempotency replays intentionally do
        // not consume an execution slot.
        req.extensions_mut().insert(auth);
        return next.run(req).await;
    }

    // 使用真实的 user_id, tenant_id, produce_ai_key_id 创建限流键
    let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);
    let tenant_id = auth.tenant_id;

    // 从数据库加载租户特定的限流配置
    let rate_limit_config = match authenticated_rate_limit_config(&state, tenant_id).await {
        Ok(config) => config,
        Err(_) => return service_unavailable_response(),
    };

    // 执行 RPM 原子检查并记录
    match state
        .rate_limiter
        .check_and_record_with_config(&rate_key, &rate_limit_config)
        .await
    {
        Ok(()) => {
            // 限流检查全部通过，继续处理请求
            req.extensions_mut().insert(auth);
            next.run(req).await
        }
        Err(keycompute_types::KeyComputeError::RateLimitExceeded(ref msg)) => {
            // 触发 RPM 限流
            info!(
                tenant_id = %rate_key.tenant_id,
                user_id = %rate_key.user_id,
                rpm_limit = rate_limit_config.rpm_limit,
                "Rate limit exceeded: {}",
                msg
            );
            rate_limit_response_for_key(&state, &rate_key, &rate_limit_config, "authenticated")
                .await
        }
        Err(e) => {
            // RPM 检查出错（如 Redis 不可用），按 fail-closed 原则拒绝请求
            error!("Rate limit check failed, denying request: {}", e);
            service_unavailable_response()
        }
    }
}

/// 对已经完成认证的 WebSocket warmup 子请求执行与 HTTP 入口相同的租户
/// RPM 限流。WebSocket 握手本身不消耗请求额度；生成事件由 Responses
/// handler 在选定上游账号后加载有效配置并在那里原子检查。
pub(crate) async fn enforce_authenticated_rate_limit(
    state: &AppState,
    auth: &AuthExtractor,
) -> Result<()> {
    let config = authenticated_rate_limit_config(state, auth.tenant_id).await?;
    enforce_authenticated_rate_limit_with_config(state, auth, &config).await
}

/// Record one generation RPM admission using an already loaded effective
/// configuration. Callers use this after all other reversible pre-dispatch
/// checks (for example TPM and balance) have succeeded.
pub(crate) async fn enforce_authenticated_rate_limit_with_config(
    state: &AppState,
    auth: &AuthExtractor,
    config: &RateLimitConfig,
) -> Result<()> {
    let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);
    enforce_authenticated_rate_limit_for_key(state, &rate_key, config).await
}

async fn enforce_authenticated_rate_limit_for_key(
    state: &AppState,
    rate_key: &RateLimitKey,
    config: &RateLimitConfig,
) -> Result<()> {
    state
        .rate_limiter
        .check_and_record_with_config(rate_key, config)
        .await
        .map_err(|error| match error {
            keycompute_types::KeyComputeError::RateLimitExceeded(_) => {
                ApiError::RateLimit("Rate limit exceeded. Please try again later.".to_string())
            }
            other => {
                error!(error = %other, "Authenticated RPM check failed, denying request");
                ApiError::ServiceUnavailable(
                    "Rate limit check failed. Please try again later.".to_string(),
                )
            }
        })
}

/// 公共认证限流中间件
///
/// 适用于无需登录的注册入口，按可信代理注入的 IP 和服务端签发 cookie 两个维度限流。
///
/// 注意：此路径仅执行 RPM 限流，不执行 TPM 预检。
/// 公共注册入口不处理 LLM 请求、不消耗 token，TPM 维度对此路径不适用。
pub async fn public_auth_rate_limit_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let headers = req.headers();
    let config = RateLimitConfig::default();
    let scope = "registration";

    let client_ip = extract_client_ip_from_headers(headers);
    if let Some(ref client_ip) = client_ip
        && let Err(response) =
            enforce_public_rate_limit(&state, scope, "ip", client_ip, &config).await
    {
        return *response;
    } else if client_ip.is_none()
        && let Err(response) =
            enforce_public_rate_limit(&state, scope, "ip-fallback", "anonymous", &config).await
    {
        return *response;
    }

    let (cookie_identity, set_cookie_header) =
        load_or_issue_public_auth_cookie(headers, state.public_auth_cookie_secret.as_str());
    if let Err(response) =
        enforce_public_rate_limit(&state, scope, "cookie", &cookie_identity, &config).await
    {
        return *response;
    }

    let mut response = next.run(req).await;
    if let Some(set_cookie_header) = set_cookie_header {
        response.headers_mut().append(SET_COOKIE, set_cookie_header);
    }

    response
}

/// 支付平台回调按可信代理提供的来源 IP 限流，避免无效验签请求放大为密码学和数据库压力。
#[derive(Clone, Debug)]
pub struct PaymentNotifyClientIp(pub String);

pub async fn payment_notify_rate_limit_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(identity) = extract_client_ip_from_headers(req.headers()) else {
        // 支付回调必须经过会覆盖 X-Real-IP 的可信代理。缺失或非法时
        // fail closed，避免直连应用的请求绕过限流。
        tracing::error!("Payment callback has no valid trusted X-Real-IP");
        return service_unavailable_response();
    };
    let scope = payment_notify_scope(req.uri().path());
    let config = RateLimitConfig::new(3000, u32::MAX);
    if let Err(response) = enforce_public_rate_limit(&state, scope, "ip", &identity, &config).await
    {
        return *response;
    }
    req.extensions_mut().insert(PaymentNotifyClientIp(identity));
    next.run(req).await
}

fn payment_notify_scope(path: &str) -> &'static str {
    if path.ends_with("/alipay") {
        "payment-notify-alipay"
    } else if path.ends_with("/wechatpay") {
        "payment-notify-wechatpay"
    } else {
        "payment-notify-unknown"
    }
}

pub(crate) fn extract_client_ip_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(real_ip) = headers.get("x-real-ip")
        && let Ok(value) = real_ip.to_str()
    {
        let ip = value.trim();
        if let Ok(parsed) = ip.parse::<std::net::IpAddr>() {
            return Some(parsed.to_string());
        }
    }

    None
}

fn load_or_issue_public_auth_cookie(
    headers: &HeaderMap,
    secret: &str,
) -> (String, Option<HeaderValue>) {
    if let Some(identity) = extract_public_auth_cookie_identity(headers, secret) {
        return (identity, None);
    }

    let identity = Uuid::new_v4().to_string();
    let signed_value = sign_public_auth_cookie_value(secret, &identity);
    let set_cookie_header = build_public_auth_set_cookie(&signed_value, request_is_secure(headers));

    (identity, set_cookie_header)
}

fn extract_public_auth_cookie_identity(headers: &HeaderMap, secret: &str) -> Option<String> {
    let raw_cookie = extract_cookie_value(headers, PUBLIC_AUTH_COOKIE_NAME)?;
    validate_public_auth_cookie_value(secret, &raw_cookie)
}

fn extract_cookie_value(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    for header_value in headers.get_all("cookie") {
        let cookie_header = header_value.to_str().ok()?;
        for part in cookie_header.split(';') {
            let trimmed = part.trim();
            let (name, cookie_value) = trimmed.split_once('=')?;
            if name.trim() == cookie_name {
                let cookie_value = cookie_value.trim();
                if !cookie_value.is_empty() {
                    return Some(cookie_value.to_string());
                }
            }
        }
    }

    None
}

fn sign_public_auth_cookie_value(secret: &str, identity: &str) -> String {
    format!(
        "{identity}.{}",
        sign_public_auth_cookie_identity(secret, identity)
    )
}

fn validate_public_auth_cookie_value(secret: &str, value: &str) -> Option<String> {
    let (identity, signature) = value.split_once('.')?;
    if identity.is_empty() || signature.is_empty() {
        return None;
    }

    let expected_signature = sign_public_auth_cookie_identity(secret, identity);
    if signature == expected_signature {
        Some(identity.to_string())
    } else {
        None
    }
}

fn sign_public_auth_cookie_identity(secret: &str, identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update("public-auth-cookie");
    hasher.update(b":");
    hasher.update(secret.as_bytes());
    hasher.update(b":");
    hasher.update(identity.as_bytes());
    hex::encode(hasher.finalize())
}

fn build_public_auth_set_cookie(value: &str, secure: bool) -> Option<HeaderValue> {
    let secure_attr = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "{PUBLIC_AUTH_COOKIE_NAME}={value}; Path=/; Max-Age={PUBLIC_AUTH_COOKIE_MAX_AGE_SECS}; HttpOnly; SameSite=Lax{secure_attr}"
    );
    HeaderValue::from_str(&cookie).ok()
}

fn request_is_secure(headers: &HeaderMap) -> bool {
    if let Some(proto) = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
    {
        return proto.eq_ignore_ascii_case("https");
    }

    false
}

async fn enforce_public_rate_limit(
    state: &AppState,
    scope: &str,
    dimension: &str,
    identity: &str,
    config: &RateLimitConfig,
) -> std::result::Result<(), Box<Response>> {
    let rate_key = build_public_rate_limit_key(scope, dimension, identity);

    match state
        .rate_limiter
        .check_and_record_with_config(&rate_key, config)
        .await
    {
        Ok(()) => Ok(()),
        Err(keycompute_types::KeyComputeError::RateLimitExceeded(ref msg)) => {
            info!(
                scope = %scope,
                dimension = %dimension,
                identity = %identity,
                rpm_limit = config.rpm_limit,
                "Public auth rate limit exceeded: {}",
                msg
            );
            Err(Box::new(
                rate_limit_response_for_key(state, &rate_key, config, scope).await,
            ))
        }
        Err(e) => {
            // 限流检查出错（如 Redis 不可用），按 fail-closed 原则拒绝请求
            error!(
                scope = %scope,
                dimension = %dimension,
                error = %e,
                "Public auth rate limit check failed, denying request"
            );
            Err(Box::new(service_unavailable_response()))
        }
    }
}

fn build_public_rate_limit_key(scope: &str, dimension: &str, identity: &str) -> RateLimitKey {
    let scope = format!("public-auth:{scope}:{dimension}");
    RateLimitKey::new(
        hash_to_uuid(&scope),
        hash_to_uuid(identity),
        hash_to_uuid(&format!("{scope}:{identity}")),
    )
}

fn hash_to_uuid(input: &str) -> Uuid {
    let digest = Sha256::digest(input.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

/// x-api-key 仅在 `/v1/messages` 路径被接受为限流身份（与认证提取器的
/// 路径限制对称，避免其他端点以 x-api-key 身份消耗配额）。
fn x_api_key_allowed_on_path(path: &str) -> bool {
    matches!(path, "/v1/messages" | "/pt/v1/messages" | "/nt/v1/messages")
}

/// Recovery is advisory and may race another caller. An unavailable metadata
/// read must not change an already-established quota rejection into admission.
async fn rate_limit_response_for_key(
    state: &AppState,
    key: &RateLimitKey,
    config: &RateLimitConfig,
    scope: &str,
) -> Response {
    let mut response = rate_limit_exceeded_response();
    if let Ok(scope) = HeaderValue::from_str(scope) {
        response.headers_mut().insert("x-ratelimit-scope", scope);
    }
    match tokio::time::timeout(
        Duration::from_millis(250),
        state.rate_limiter.rpm_retry_after(key, config),
    )
    .await
    {
        Ok(Ok(Some(remaining))) => {
            // Round upwards: HTTP delay-seconds must not ask clients to retry
            // before a sub-second fixed window actually expires.
            let seconds = remaining
                .as_secs()
                .saturating_add(u64::from(remaining.subsec_nanos() > 0))
                .max(1);
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert("retry-after", value);
            }
        }
        // No current blocking window, or an extension backend without metadata.
        // Omit the header rather than fabricating a recovery timestamp.
        Ok(Ok(None)) => {}
        _ => warn!(
            "RPM recovery metadata unavailable; retaining quota rejection without a fabricated reset"
        ),
    }
    response
}

fn rate_limit_exceeded_response() -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        serde_json::json!({
            "error": {
                "message": "Rate limit exceeded. Please try again later.",
                "type": "rate_limit_exceeded",
                "code": "rate_limit_exceeded"
            }
        })
        .to_string(),
    )
        .into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response.extensions_mut().insert(TrustedLocalApiError);
    response
}

/// 权限检查中间件
///
/// 检查用户是否具有指定的权限
/// Only the permissions of the authenticated credential are considered.
pub async fn require_permission(
    State(_state): State<AppState>,
    auth: AuthExtractor,
    req: Request,
    next: Next,
    required_permission: Permission,
) -> Result<Response> {
    use keycompute_auth::PermissionChecker;

    // 权限检查完全基于 AuthContext 中已构建的权限列表
    // 权限在认证时已根据认证类型(API Key/JWT)和角色正确构建
    let user_permissions = auth.permissions.clone();

    if !PermissionChecker::check(
        auth.credential_kind,
        &user_permissions,
        &required_permission,
    ) {
        return Err(ApiError::Auth(format!(
            "Permission denied: requires {:?}",
            required_permission
        )));
    }

    Ok(next.run(req).await)
}

/// 创建权限检查中间件层
///
/// 使用示例：
/// ```rust,ignore
/// // 在路由中使用权限中间件
/// Router::new()
///     .route("/api/v1/users", get(list_users))
///     .layer(from_fn_with_state(state.clone(), |state, auth, req, next| {
///         permission_middleware(state, auth, req, next, Permission::ManageUsers)
///     }))
/// ```
#[allow(clippy::type_complexity)]
pub fn permission_middleware(
    permission: Permission,
) -> impl Fn(
    State<AppState>,
    AuthExtractor,
    Request,
    Next,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response>> + Send>>
+ Clone {
    move |state: State<AppState>, auth: AuthExtractor, req: Request, next: Next| {
        let perm = permission;
        Box::pin(async move { require_permission(state, auth, req, next, perm).await })
    }
}

// ==================== Admin 认证中间件 ====================

/// Admin 认证中间件
///
/// 专为 Admin 路由设计，提供统一的权限保护：
/// 1. 验证请求是否携带有效的认证 Token
/// 2. 检查用户是否具有 Admin 角色
/// 3. 将认证信息注入请求扩展，供后续 Handler 使用
///
/// # 返回
/// - 成功：继续处理请求
/// - 401：未认证或认证失败
/// - 403：认证成功但非 Admin 角色
///
/// # 使用示例
/// ```rust,ignore
/// let admin_routes = Router::new()
///     .route("/api/v1/users", get(list_all_users))
///     .layer(from_fn_with_state(state.clone(), admin_auth_middleware));
/// ```
pub async fn admin_auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let context = if let Some(context) = req
        .extensions()
        .get::<keycompute_auth::AuthContext>()
        .cloned()
    {
        context
    } else if let Some(auth) = req.extensions().get::<AuthExtractor>() {
        auth.authorization_context()
    } else {
        let Some(token) = req
            .headers()
            .get("Authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
        else {
            return ApiError::Auth("Authentication required".into()).into_response();
        };
        match state.auth.verify_token(token).await {
            Ok(context) => context,
            Err(keycompute_types::KeyComputeError::DatabaseError(_))
            | Err(keycompute_types::KeyComputeError::ServiceUnavailable(_)) => {
                return authentication_service_unavailable_response();
            }
            Err(error) => return ApiError::from(error).into_response(),
        }
    };
    if let Err(error) =
        context.require_platform(keycompute_auth::AuthorizationAction::ManagePlatform)
    {
        return ApiError::from(error).into_response();
    }
    let global = match crate::extractors::GlobalConsoleAuth::try_from(context.clone()) {
        Ok(global) => global,
        Err(error) => return error.into_response(),
    };
    req.extensions_mut().insert(context.clone());
    req.extensions_mut().insert(global);
    // Tenant-dependent handlers still require an explicit active membership;
    // platform identity/lifecycle handlers use the global context directly.
    if context.selected_tenant_id.is_some() {
        match AuthExtractor::from_auth_context(context) {
            Ok(auth) => {
                req.extensions_mut().insert(auth);
            }
            Err(error) => return error.into_response(),
        }
    }
    crate::console::run_with_mutation_fence(&state, req, next).await
}

/// 从请求扩展中提取 AuthExtractor
///
/// 用于在 Handler 中获取已由中间件验证的认证信息
///
/// # 使用示例
/// ```rust,ignore
/// pub async fn admin_handler(
///     Extension(auth): Extension<AuthExtractor>,
/// ) -> Result<Json<...>> {
///     // auth 已由 admin_auth_middleware 验证
///     Ok(Json(...))
/// }
/// ```
pub fn extract_auth_from_extensions(req: &Request) -> Option<AuthExtractor> {
    req.extensions().get::<AuthExtractor>().cloned()
}

// ==================== 维护模式中间件 ====================

const DEFAULT_MAINTENANCE_MESSAGE: &str = "System is under maintenance. Please try again later.";

async fn maintenance_mode_enabled(state: &AppState) -> bool {
    use keycompute_db::models::system_setting::setting_keys;

    if let Some(pool) = state.pool.as_deref() {
        keycompute_db::SystemSetting::get_bool(
            pool.write_conn(),
            setting_keys::MAINTENANCE_MODE,
            false,
        )
        .await
    } else {
        false
    }
}

async fn maintenance_mode_message(state: &AppState) -> String {
    use keycompute_db::models::system_setting::setting_keys;

    if let Some(pool) = state.pool.as_deref() {
        keycompute_db::SystemSetting::get_string(
            pool.write_conn(),
            setting_keys::MAINTENANCE_MESSAGE,
            DEFAULT_MAINTENANCE_MESSAGE,
        )
        .await
    } else {
        DEFAULT_MAINTENANCE_MESSAGE.to_string()
    }
}

fn maintenance_mode_allows_request(is_maintenance: bool, is_system_admin: bool) -> bool {
    !is_maintenance || is_system_admin
}

/// Recheck maintenance mode for authenticated work that does not traverse the
/// HTTP middleware on every operation, such as messages on an established
/// WebSocket connection.
pub(crate) async fn enforce_authenticated_maintenance_mode(
    state: &AppState,
    auth: &AuthExtractor,
) -> Result<()> {
    let is_maintenance = maintenance_mode_enabled(state).await;
    let is_system_admin = auth.has_permission(&Permission::ManageProtectedUsers);
    if maintenance_mode_allows_request(is_maintenance, is_system_admin) {
        if is_maintenance {
            info!(
                user_id = %auth.user_id,
                "Admin bypassing maintenance mode"
            );
        }
        return Ok(());
    }

    Err(ApiError::ServiceUnavailable(
        maintenance_mode_message(state).await,
    ))
}

/// 维护模式中间件
///
/// 检查系统是否处于维护模式：
/// 1. 读取 system_settings 表中的 maintenance_mode 设置
/// 2. 如果启用维护模式，返回 503 Service Unavailable
/// 3. 管理员（admin 角色）可以绕过维护模式继续访问
///
/// # 排除路径
/// 以下路径不受维护模式影响：
/// - /health - 健康检查
/// - /api/v1/settings/public - 公开设置（前端需要获取维护状态）
/// - /api/v1/auth/login - 登录（管理员需要登录）
/// - /api/v1/auth/refresh-token - 刷新登录状态
/// - /api/v1/payments/notify/* - 支付平台回调（仍由验签和专用限流保护）
///
/// # 使用示例
/// ```rust,ignore
/// Router::new()
///     .layer(from_fn_with_state(state.clone(), maintenance_mode_middleware));
/// ```
pub async fn maintenance_mode_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    // 排除不需要维护模式检查的路径
    let path = req.uri().path();
    if is_maintenance_excluded_path(path) {
        return next.run(req).await;
    }

    // 检查维护模式状态
    let is_maintenance = maintenance_mode_enabled(&state).await;

    if !is_maintenance {
        return next.run(req).await;
    }

    // 维护模式已启用，检查是否为管理员
    // 从请求头提取认证信息
    let is_system_admin = if let Some(auth) = req.extensions().get::<AuthExtractor>() {
        auth.has_permission(&Permission::ManageProtectedUsers)
    } else if let Some(auth_header) = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        && let Some(token) = auth_header.strip_prefix("Bearer ")
        && let Ok(auth_context) = state.auth.verify_token(token).await
        && auth_context.has_permission(&Permission::ManageProtectedUsers)
    {
        info!(
            user_id = %auth_context.user_id,
            "Admin bypassing maintenance mode"
        );
        true
    } else {
        false
    };
    if maintenance_mode_allows_request(is_maintenance, is_system_admin) {
        // 管理员绕过维护模式（基于权限判断，API Key 无 ManageProtectedUsers 权限，无法绕过）
        return next.run(req).await;
    }

    // 获取维护消息
    let maintenance_message = maintenance_mode_message(&state).await;

    warn!(
        path = %request_log_path(req.uri()),
        "Request blocked due to maintenance mode"
    );

    maintenance_mode_response(maintenance_message)
}

fn maintenance_mode_response(maintenance_message: String) -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({
            "error": {
                "message": maintenance_message,
                "type": "maintenance_mode",
                "code": "service_unavailable"
            }
        })
        .to_string(),
    )
        .into_response();
    response
        .extensions_mut()
        .insert(TrustedPublicMaintenanceError);
    response
}

fn is_maintenance_excluded_path(path: &str) -> bool {
    matches!(
        path,
        "/health"
            | "/api/v1/settings/public"
            | "/api/v1/auth/login"
            | "/api/v1/auth/refresh-token"
            | "/api/v1/payments/notify/alipay"
            | "/api/v1/payments/notify/wechatpay"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn capability_paths_and_query_values_never_enter_request_logs() {
        for path in [
            "/api/v1/invitations/private-token/accept?token=private-query",
            "/api/v1/auth/verify-reset-token/private-token?token=private-query",
        ] {
            let uri = path.parse().unwrap();
            let safe = super::request_log_path(&uri);
            assert!(!safe.contains("private-token"));
            assert!(!safe.contains("private-query"));
            assert!(safe.contains("[redacted]"));
        }
        let uri = "/api/v1/me/profile?email=private-email".parse().unwrap();
        assert_eq!(super::request_log_path(&uri), "/api/v1/me/profile");
    }

    use super::*;
    use crate::state::{AppStateConfig, JwtConfig};
    use axum::http::Request;
    use axum::{
        Json, Router,
        body::Body,
        middleware::{from_fn, from_fn_with_state},
        routing::{get, post},
    };
    use keycompute_auth::JwtValidator;
    use keycompute_types::CredentialKind;
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Poll,
    };
    use tower::ServiceExt;
    use uuid::Uuid;

    async fn delayed_received_at_echo(RequestReceivedAt(received_at): RequestReceivedAt) -> String {
        received_at.to_rfc3339()
    }

    async fn delay_after_ingress(req: axum::extract::Request, next: Next) -> Response {
        tokio::time::sleep(Duration::from_millis(40)).await;
        next.run(req).await
    }

    fn stalled_body(polls: Arc<AtomicUsize>) -> Body {
        Body::from_stream(futures::stream::poll_fn(move |_| {
            polls.fetch_add(1, Ordering::Relaxed);
            Poll::<Option<std::result::Result<bytes::Bytes, Infallible>>>::Pending
        }))
    }

    #[test]
    fn generation_body_admission_targets_only_generation_endpoints() {
        assert!(generation_request_needs_body_admission(
            &Method::POST,
            "/v1/responses",
            None,
        ));
        assert!(generation_request_needs_body_admission(
            &Method::POST,
            "/v1/responses/input_tokens",
            Some(GENERATION_LARGE_HTTP_BODY_BYTES + 1),
        ));
        assert!(!generation_request_needs_body_admission(
            &Method::POST,
            "/v1/responses",
            Some(GENERATION_LARGE_HTTP_BODY_BYTES),
        ));
        assert!(!generation_request_needs_body_admission(
            &Method::POST,
            "/v1/responses/resp_123/cancel",
            None,
        ));
        assert!(!generation_request_needs_body_admission(
            &Method::GET,
            "/v1/responses",
            None,
        ));

        let chat = generation_json_body_policy(&Method::POST, "/v1/chat/completions")
            .expect("Chat Completions must use bounded JSON admission");
        assert_eq!(chat.body_limit_bytes, OPENAI_CHAT_BODY_LIMIT_BYTES);
        assert_eq!(
            chat.working_set_limit_bytes,
            OPENAI_CHAT_REQUEST_WORKING_SET_LIMIT_BYTES
        );
        assert!(generation_request_needs_body_admission(
            &Method::POST,
            "/v1/chat/completions",
            None,
        ));
        let messages = generation_json_body_policy(&Method::POST, "/v1/messages")
            .expect("Anthropic Messages must use bounded JSON admission");
        assert_eq!(
            messages.body_limit_bytes,
            ANTHROPIC_MESSAGES_BODY_LIMIT_BYTES
        );
        assert_eq!(
            messages.working_set_limit_bytes,
            ANTHROPIC_MESSAGES_REQUEST_WORKING_SET_LIMIT_BYTES
        );
    }

    #[test]
    fn generation_body_admission_rejects_dense_json_before_deserialization() {
        let string_heavy = br#"{"input":"0,0,0,0"}"#;
        let array_heavy = br#"{"input":[0,0,0,0]}"#;
        let threshold = estimated_json_parse_working_set_bytes(string_heavy);

        assert_eq!(
            generation_json_body_needs_working_set_admission(string_heavy, usize::MAX, threshold,),
            Ok(false)
        );
        assert_eq!(
            generation_json_body_needs_working_set_admission(array_heavy, usize::MAX, threshold,),
            Ok(true)
        );
        assert_eq!(
            generation_json_body_needs_working_set_admission(
                array_heavy,
                estimated_json_parse_working_set_bytes(array_heavy) - 1,
                threshold,
            ),
            Err(())
        );

        const OFFICIAL_INLINE_SKILL_BASE64_MAX: usize = 70_254_592;
        const {
            assert!(
                OFFICIAL_INLINE_SKILL_BASE64_MAX * 2 + 1024
                    < OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES
            );
        }
    }

    #[tokio::test]
    async fn generation_body_middleware_rejects_dense_json_at_production_limit() {
        let item_count = OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES / 64 + 1;
        let mut payload = String::with_capacity(item_count * 2 + 1);
        payload.push('[');
        for index in 0..item_count {
            if index > 0 {
                payload.push(',');
            }
            payload.push('0');
        }
        payload.push(']');
        assert!(
            estimated_json_parse_working_set_bytes(payload.as_bytes())
                > OPENAI_RESPONSES_REQUEST_WORKING_SET_LIMIT_BYTES
        );

        let state = AppState::new();
        let app = Router::new()
            .route("/v1/responses", post(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(
                state.clone(),
                generation_http_body_admission_middleware,
            ))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/responses")
                    .extension(AuthExtractor::new(
                        Uuid::new_v4(),
                        Uuid::new_v4(),
                        Uuid::new_v4(),
                        keycompute_types::CredentialKind::Jwt,
                    ))
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn small_chunked_generation_bodies_do_not_hold_resident_body_permits() {
        let state = AppState::new();
        let barrier = Arc::new(tokio::sync::Barrier::new(4));
        let handler_barrier = Arc::clone(&barrier);
        let app = Router::new()
            .route(
                "/v1/responses",
                post(
                    move |permit: Option<
                        axum::Extension<crate::state::GenerationHttpBodyPermit>,
                    >| {
                        let barrier = Arc::clone(&handler_barrier);
                        async move {
                            assert!(
                                permit.as_ref().is_some_and(|guard| !guard.0.owns_large_slot()),
                                "a small body keeps its byte claim but releases the provisional large-object slot"
                            );
                            barrier.wait().await;
                            StatusCode::NO_CONTENT
                        }
                    },
                ),
            )
            .layer(from_fn_with_state(
                state.clone(),
                generation_http_body_admission_middleware,
            ))
            .with_state(state);

        let mut requests = Vec::new();
        for _ in 0..3 {
            let app = app.clone();
            requests.push(tokio::spawn(async move {
                let body = Body::from_stream(futures::stream::once(async {
                    Ok::<_, Infallible>(bytes::Bytes::from_static(b"{}"))
                }));
                app.oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/v1/responses")
                        .extension(AuthExtractor::new(
                            Uuid::new_v4(),
                            Uuid::new_v4(),
                            Uuid::new_v4(),
                            keycompute_types::CredentialKind::Jwt,
                        ))
                        .body(body)
                        .unwrap(),
                )
                .await
                .unwrap()
            }));
        }

        tokio::time::timeout(Duration::from_secs(5), barrier.wait())
            .await
            .expect("three small chunked requests should reach the handler concurrently");
        for request in requests {
            assert_eq!(request.await.unwrap().status(), StatusCode::NO_CONTENT);
        }
    }

    #[tokio::test]
    async fn large_chunked_generation_body_retains_its_resident_body_permit() {
        let state = AppState::new();
        let app =
            Router::new()
                .route(
                    "/v1/responses",
                    post(
                        |permit: Option<
                            axum::Extension<crate::state::GenerationHttpBodyPermit>,
                        >| async move {
                            if permit.is_some() {
                                StatusCode::NO_CONTENT
                            } else {
                                StatusCode::INTERNAL_SERVER_ERROR
                            }
                        },
                    ),
                )
                .layer(from_fn_with_state(
                    state.clone(),
                    generation_http_body_admission_middleware,
                ))
                .with_state(state);
        let mut payload = vec![b'a'; GENERATION_LARGE_HTTP_BODY_BYTES as usize];
        payload.insert(0, b'"');
        payload.push(b'"');
        let body = Body::from_stream(futures::stream::once(async {
            Ok::<_, Infallible>(bytes::Bytes::from(payload))
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/responses")
                    .extension(AuthExtractor::new(
                        Uuid::new_v4(),
                        Uuid::new_v4(),
                        Uuid::new_v4(),
                        keycompute_types::CredentialKind::Jwt,
                    ))
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn responses_body_middleware_authenticates_before_polling_body() {
        for authorization in [None, Some("Bearer invalid-token")] {
            let state = AppState::new();
            let app = Router::new()
                .route("/v1/responses", post(|| async { StatusCode::NO_CONTENT }))
                .layer(from_fn_with_state(
                    state.clone(),
                    generation_http_body_admission_middleware,
                ))
                .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
                .with_state(state);
            let polls = Arc::new(AtomicUsize::new(0));
            let mut request = Request::builder().method(Method::POST).uri("/v1/responses");
            if let Some(authorization) = authorization {
                request = request.header("Authorization", authorization);
            }

            let response = tokio::time::timeout(
                Duration::from_secs(1),
                app.oneshot(request.body(stalled_body(Arc::clone(&polls))).unwrap()),
            )
            .await
            .expect("authentication should reject without waiting for the body")
            .unwrap();

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(polls.load(Ordering::Relaxed), 0);
        }
    }

    #[tokio::test]
    async fn responses_body_read_has_a_total_timeout() {
        let polls = Arc::new(AtomicUsize::new(0));
        let result = read_generation_http_body(
            stalled_body(Arc::clone(&polls)),
            OPENAI_RESPONSES_BODY_LIMIT_BYTES,
            Duration::from_millis(10),
        )
        .await;

        assert_eq!(result, Err(GenerationHttpBodyReadError::Timeout));
        assert!(polls.load(Ordering::Relaxed) > 0);
    }

    #[tokio::test]
    async fn authenticated_rate_limit_tenant_lookup_failure_denies_before_recording_rpm() {
        let state = AppState::with_pool(keycompute_db::DbRouter::single(
            sea_orm::DatabaseConnection::Disconnected,
        ));
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);

        let result = enforce_authenticated_rate_limit(&state, &auth).await;

        assert!(matches!(result, Err(ApiError::ServiceUnavailable(_))));
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn account_rate_limit_config_uses_defaults_without_pool() {
        let state = AppState::new();
        let default_config = RateLimitConfig::default();
        let tenant_id = Uuid::new_v4();

        assert_eq!(
            authenticated_rate_limit_config_for_account(&state, tenant_id, Some(Uuid::new_v4()))
                .await
                .unwrap()
                .rpm_limit,
            default_config.rpm_limit
        );
        assert_eq!(
            authenticated_rate_limit_config_for_account(&state, tenant_id, Some(Uuid::new_v4()))
                .await
                .unwrap()
                .tpm_limit,
            default_config.tpm_limit
        );
        assert_eq!(
            authenticated_rate_limit_config_for_account(&state, tenant_id, None)
                .await
                .unwrap()
                .rpm_limit,
            default_config.rpm_limit
        );
        assert_eq!(
            authenticated_rate_limit_config_for_account(&state, tenant_id, None)
                .await
                .unwrap()
                .tpm_limit,
            default_config.tpm_limit
        );
    }

    #[tokio::test]
    async fn account_rate_limit_config_denies_when_lookup_fails() {
        let state = AppState::with_pool(keycompute_db::DbRouter::single(
            sea_orm::DatabaseConnection::Disconnected,
        ));
        let result = authenticated_rate_limit_config_for_account(
            &state,
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
        )
        .await;

        assert!(matches!(result, Err(ApiError::ServiceUnavailable(_))));
    }

    #[tokio::test]
    async fn enforce_rate_limit_with_config_uses_defaults_without_pool() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);
        let config = authenticated_rate_limit_config_for_account(
            &state,
            auth.tenant_id,
            Some(Uuid::new_v4()),
        )
        .await
        .unwrap();
        assert!(
            enforce_authenticated_rate_limit_with_config(&state, &auth, &config)
                .await
                .is_ok()
        );
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn config_lookup_failure_denies_without_recording() {
        let state = AppState::with_pool(keycompute_db::DbRouter::single(
            sea_orm::DatabaseConnection::Disconnected,
        ));
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);

        let config_result = authenticated_rate_limit_config_for_account(
            &state,
            auth.tenant_id,
            Some(Uuid::new_v4()),
        )
        .await;

        assert!(matches!(
            config_result,
            Err(ApiError::ServiceUnavailable(_))
        ));
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            0
        );
    }

    #[test]
    fn account_limits_can_only_tighten_the_tenant_limit() {
        let tenant = RateLimitConfig::new(25, 2_000);
        let account = RateLimitConfig::new(100, 10_000);
        let effective = stricter_rate_limit_config(tenant, account);

        assert_eq!(effective.rpm_limit, 25);
        assert_eq!(effective.tpm_limit, 2_000);

        let tighter_account = RateLimitConfig::new(5, 500);
        let effective = stricter_rate_limit_config(effective, tighter_account);
        assert_eq!(effective.rpm_limit, 5);
        assert_eq!(effective.tpm_limit, 500);
    }

    #[tokio::test]
    async fn console_rpm_response_reports_backend_recovery_and_is_not_cacheable() {
        let state = AppState::new();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::nil());
        let config = RateLimitConfig::new(1, 1000);
        state
            .rate_limiter
            .check_and_record_with_config(&key, &config)
            .await
            .unwrap();
        let response = rate_limit_response_for_key(&state, &key, &config, "authenticated").await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-ratelimit-scope"], "authenticated");
        let delay: u64 = response.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=60).contains(&delay));
        assert_eq!(state.rate_limiter.get_rpm_count(&key).await.unwrap(), 1);
        let unknown = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::nil());
        let response =
            rate_limit_response_for_key(&state, &unknown, &config, "authenticated").await;
        assert!(!response.headers().contains_key("retry-after"));
    }

    #[tokio::test]
    async fn rate_limit_middleware_reuses_cached_auth_and_records_rpm() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);
        let app = Router::new()
            .route("/rate-limited", post(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state.clone());

        // An outer authentication layer may have consumed the credentials or
        // may leave a malformed header behind. The verified extension remains
        // authoritative and must still consume the authenticated RPM quota.
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rate-limited")
                    .header("Authorization", "not-a-bearer-header")
                    .extension(auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            1
        );
    }

    async fn assert_canonical_chat_rate_limit_response(response: Response) {
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        // Local quota decisions do not expose a trustworthy remaining-window
        // duration. Do not manufacture Retry-After; native upstream 429s keep
        // the provider's value in `native_openai_error_response`.
        assert!(response.headers().get("retry-after").is_none());
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "error": {
                    "message": "Rate limit exceeded. Please try again later.",
                    "type": "rate_limit_error",
                    "param": null,
                    "code": "rate_limit_exceeded",
                }
            })
        );
    }

    #[tokio::test]
    async fn chat_route_rpm_is_not_enforced_by_transport_middleware() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);
        let config = RateLimitConfig::default();
        for _ in 0..config.rpm_limit {
            state
                .rate_limiter
                .check_and_record_with_config(&rate_key, &config)
                .await
                .unwrap();
        }
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async { StatusCode::NO_CONTENT }),
            )
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .extension(auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            u64::from(config.rpm_limit)
        );
    }

    #[tokio::test]
    async fn generic_rest_rpm_rejection_keeps_generic_rate_limit_contract() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);
        let config = RateLimitConfig::default();
        for _ in 0..config.rpm_limit {
            state
                .rate_limiter
                .check_and_record_with_config(&rate_key, &config)
                .await
                .unwrap();
        }
        let app = Router::new()
            .route("/api/v1/me", get(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/me")
                    .extension(auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "rate_limit_exceeded");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert!(body["error"].get("param").is_none());
    }

    #[tokio::test]
    async fn openai_rate_limit_normalizer_leaves_untrusted_upstream_429_unchanged() {
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "7")],
                        Json(serde_json::json!({
                            "error": {
                                "message": "sanitized upstream failure",
                                "type": "provider_rate_limit",
                                "code": "provider_limit"
                            }
                        })),
                    )
                }),
            )
            .layer(from_fn(openai_rate_limit_response_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "7");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "provider_rate_limit");
        assert_eq!(body["error"]["code"], "provider_limit");
    }

    #[tokio::test]
    async fn openai_rate_limit_normalizer_preserves_trusted_local_headers() {
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async {
                    let mut response = rate_limit_exceeded_response();
                    response
                        .headers_mut()
                        .insert("x-request-id", HeaderValue::from_static("req_local_123"));
                    response
                }),
            )
            .layer(from_fn(openai_rate_limit_response_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.headers()["x-request-id"], "req_local_123");
        assert_canonical_chat_rate_limit_response(response).await;
    }

    #[tokio::test]
    async fn chat_route_tpm_rejection_uses_canonical_openai_rate_limit_response() {
        let state = AppState::new();
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);
        state
            .rate_limiter
            .record_token_usage(&rate_key, keycompute_ratelimit::DEFAULT_TPM_LIMIT)
            .await
            .unwrap();
        let ctx = keycompute_types::RequestContext::new(
            Uuid::new_v4(),
            auth.user_id,
            auth.tenant_id,
            auth.produce_ai_key_id,
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        let handler_state = state.clone();
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(move || {
                    let state = handler_state.clone();
                    let ctx = ctx.clone();
                    async move {
                        crate::handlers::reserve_generation_tpm(
                            &state,
                            &ctx,
                            keycompute_ratelimit::RateLimitConfig::default(),
                        )
                        .await
                        .map(|_reservation| StatusCode::NO_CONTENT)
                    }
                }),
            )
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .layer(from_fn(openai_rate_limit_response_middleware))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .extension(auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_canonical_chat_rate_limit_response(response).await;
    }

    #[tokio::test]
    async fn rate_limit_middleware_fails_closed_on_authentication_backend_error() {
        let state = AppState::with_pool(keycompute_db::DbRouter::single(
            sea_orm::DatabaseConnection::Disconnected,
        ));
        let jwt = JwtConfig::default();
        let token = JwtValidator::new(&jwt.secret, &jwt.issuer)
            .generate_identity_token(
                Uuid::new_v4(),
                Some(Uuid::new_v4()),
                0,
                Some(1),
                Some(1),
                3600,
            )
            .unwrap();
        let app = Router::new()
            .route("/rate-limited", get(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/rate-limited")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "authentication_unavailable");
    }

    #[tokio::test]
    async fn middleware_defers_all_tpm_admission_until_the_generation_handler() {
        let fixture = crate::test_support::TestIdentity::member().await;
        let state = fixture.state.clone();
        let token = fixture.token.clone();
        let auth = state.auth.verify_token(&token).await.unwrap();
        let tenant_id = auth
            .selected_tenant_id
            .expect("scoped response test token must carry a selected tenant");
        let rate_key = RateLimitKey::new(tenant_id, auth.user_id, auth.produce_ai_key_id);
        state
            .rate_limiter
            .record_token_usage(&rate_key, keycompute_ratelimit::DEFAULT_TPM_LIMIT)
            .await
            .unwrap();
        let app = Router::new()
            .route("/v1/responses", post(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state.clone());

        let replay_candidate = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/responses")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Idempotency-Key", "completed-request")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay_candidate.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            0
        );

        let new_request = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/responses")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(new_request.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            0
        );
        assert_eq!(
            state.rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            u64::from(keycompute_ratelimit::DEFAULT_TPM_LIMIT),
            "the transport middleware must not mutate TPM state"
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn websocket_event_transport_check_records_only_rpm() {
        let state = AppState::with_config(AppStateConfig::default());
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        );
        let rate_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);
        state
            .rate_limiter
            .record_token_usage(&rate_key, keycompute_ratelimit::DEFAULT_TPM_LIMIT)
            .await
            .unwrap();

        enforce_authenticated_rate_limit(&state, &auth)
            .await
            .expect("event transport admission should remain available above TPM");
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            1
        );
        enforce_authenticated_rate_limit(&state, &auth)
            .await
            .expect("the parsed generation handler owns TPM admission");
        assert_eq!(
            state.rate_limiter.get_rpm_count(&rate_key).await.unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn test_cors_layer() {
        let cors = cors_layer();
        // 确保可以创建 CORS 层
        let _ = cors;
    }

    #[tokio::test]
    async fn responses_errors_use_openai_schema_and_redact_official_upstream_messages() {
        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(|| async {
                    Err::<(), _>(ApiError::BadRequest("model is required".to_string()))
                }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["message"], "model is required");
        assert!(body["error"]["param"].is_null());
        assert!(body["error"]["code"].is_null());

        // The Responses outer normalizer must recognize the generic local
        // numeric status as trusted, preserve its safe message, and map a
        // handler-side TPM decision onto the OpenAI-compatible 429 contract.
        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(|| async {
                    Err::<(), _>(ApiError::RateLimit(
                        "Rate limit exceeded. Please try again later.".to_string(),
                    ))
                }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert!(body["error"]["param"].is_null());
        assert_eq!(
            body["error"]["message"],
            "Rate limit exceeded. Please try again later."
        );

        // Transport RPM rejection uses the long-standing generic middleware
        // envelope before the Responses route-specific normalizer sees it.
        let app = Router::new()
            .route(
                "/v1/responses",
                post(|| async { rate_limit_exceeded_response() }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert_eq!(
            body["error"]["message"],
            "Rate limit exceeded. Please try again later."
        );

        let official = r#"{"error":{"message":"slow down","type":"rate_limit_error","param":null,"code":"rate_limit_exceeded"}}"#;
        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(move || async move {
                    Response::builder()
                        .status(StatusCode::TOO_MANY_REQUESTS)
                        .body(Body::from(official))
                        .unwrap()
                }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert!(body["error"]["param"].is_null());
        assert_eq!(body["error"]["message"], "Upstream request failed");
        assert!(!body.to_string().contains("slow down"));

        // A compatible upstream can imitate ApiError's numeric code shape.
        // Trust is carried in response extensions, never inferred from JSON.
        let numeric_code_upstream = r#"{"error":{"message":"host=db.internal password=secret","type":"rate_limit_error","param":null,"code":429}}"#;
        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(move || async move {
                    Response::builder()
                        .status(StatusCode::TOO_MANY_REQUESTS)
                        .body(Body::from(numeric_code_upstream))
                        .unwrap()
                }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["message"], "Request failed");
        assert!(!body.to_string().contains("db.internal"));
        assert!(!body.to_string().contains("secret"));

        let untrusted_classification = r#"{"error":{"message":"failed","type":"credential_sk_live_secret","param":"authorization_token","code":"sk_live_secret"}}"#;
        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(move || async move {
                    Response::builder()
                        .status(StatusCode::TOO_MANY_REQUESTS)
                        .body(Body::from(untrusted_classification))
                        .unwrap()
                }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert!(body["error"]["param"].is_null());
        assert!(!body.to_string().contains("secret"));
        assert!(!body.to_string().contains("authorization_token"));

        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(|| async {
                    Err::<(), _>(ApiError::ServiceUnavailable(
                        "database host=db.internal password=secret".to_string(),
                    ))
                }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["message"], "Request failed");
        assert!(!body.to_string().contains("db.internal"));
        assert!(!body.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn responses_errors_preserve_only_marked_public_maintenance_messages() {
        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(|| async {
                    maintenance_mode_response("Planned maintenance".to_string())
                }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "maintenance_mode");
        assert_eq!(body["error"]["code"], "service_unavailable");
        assert_eq!(body["error"]["message"], "Planned maintenance");

        let unmarked = r#"{"error":{"message":"Planned maintenance","type":"maintenance_mode","code":"service_unavailable"}}"#;
        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(move || async move {
                    Response::builder()
                        .status(StatusCode::SERVICE_UNAVAILABLE)
                        .body(Body::from(unmarked))
                        .unwrap()
                }),
            )
            .layer(from_fn(openai_responses_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["message"], "Upstream request failed");
    }

    #[test]
    fn client_request_id_validation_is_strict_ascii() {
        assert_eq!(
            validate_client_request_id("client.abc_123:-"),
            Some("client.abc_123:-".to_string())
        );
        assert_eq!(validate_client_request_id(""), None);
        assert_eq!(validate_client_request_id("contains space"), None);
        assert_eq!(validate_client_request_id("请求"), None);
        assert_eq!(validate_client_request_id(&"x".repeat(129)), None);
    }

    #[tokio::test]
    async fn response_returns_canonical_and_client_request_id_headers() {
        let state = AppState::with_config(AppStateConfig::default());
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(state.clone(), trace_id_middleware))
            .layer(cors_layer())
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("Origin", "https://client.example")
                    .header("X-Request-ID", "client-id_123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let internal = response.headers()["X-Request-ID"].to_str().unwrap();
        assert!(Uuid::parse_str(internal).is_ok());
        assert_eq!(response.headers()["X-Client-Request-ID"], "client-id_123");
        assert!(response.headers().get("X-KeyCompute-Request-ID").is_none());
        let exposed = response.headers()["Access-Control-Expose-Headers"]
            .to_str()
            .unwrap()
            .split(',')
            .map(|header| header.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        assert!(exposed.iter().any(|header| header == "x-request-id"));
        assert!(exposed.iter().any(|header| header == "x-client-request-id"));
    }

    #[tokio::test]
    async fn trace_middleware_captures_received_at_before_downstream_work() {
        let state = AppState::with_config(AppStateConfig::default());
        let app = Router::new()
            .route("/", get(delayed_received_at_echo))
            .layer(from_fn(delay_after_ingress))
            .layer(from_fn_with_state(state.clone(), trace_id_middleware))
            .with_state(state);
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let completed_at = chrono::Utc::now();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let received_at = chrono::DateTime::parse_from_rfc3339(
            std::str::from_utf8(&body).expect("timestamp response is UTF-8"),
        )
        .unwrap()
        .with_timezone(&chrono::Utc);
        assert!(
            completed_at - received_at >= chrono::Duration::milliseconds(30),
            "received_at must include time spent in downstream middleware"
        );
    }

    #[tokio::test]
    async fn anthropic_error_middleware_wraps_generic_errors() {
        let app = Router::new()
            .route(
                "/v1/messages",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": {"message": "messages is required", "type": "bad_request_error"}
                        })),
                    )
                }),
            )
            .layer(from_fn(anthropic_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["message"], "messages is required");
    }

    #[tokio::test]
    async fn anthropic_error_middleware_passes_through_conforming_schema() {
        // 已经符合 Anthropic Errors schema 的错误响应不得被二次封装：SDK 依赖
        // 顶层 `type: "error"` 与 `error` 对象，重复包装会让结构化解析失效。
        let app = Router::new()
            .route(
                "/v1/messages",
                get(|| async {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({
                            "type": "error",
                            "error": {
                                "type": "authentication_error",
                                "message": "invalid x-api-key"
                            }
                        })),
                    )
                }),
            )
            .layer(from_fn(anthropic_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "authentication_error");
        assert_eq!(body["error"]["message"], "invalid x-api-key");
        assert!(body.get("error").is_some());
        assert_eq!(body.as_object().unwrap().len(), 2, "must not be re-wrapped");
    }

    #[tokio::test]
    async fn anthropic_error_middleware_passes_through_redirects() {
        // 3xx 携带 Location 等跳转语义，改写成错误 JSON 会破坏重定向流程，
        // 必须原样透传（含状态码与响应头）。
        let app = Router::new()
            .route(
                "/v1/messages",
                get(|| async { (StatusCode::FOUND, [("Location", "/login")], "redirecting") }),
            )
            .layer(from_fn(anthropic_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers().get("location").unwrap(), "/login");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(body.as_ref(), b"redirecting");
    }

    #[tokio::test]
    async fn anthropic_error_middleware_truncates_plaintext_error_line() {
        // 纯文本 400 首行超过长度上限时必须被截断，不能完整回显到响应。
        let long_line = format!(
            "Failed to deserialize the JSON body into the target type: missing field `{}`",
            "x".repeat(600)
        );
        let app = Router::new()
            .route(
                "/v1/messages",
                get(move || async move { (StatusCode::BAD_REQUEST, long_line) }),
            )
            .layer(from_fn(anthropic_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.len() <= MAX_PLAINTEXT_ERROR_CHARS,
            "plaintext error line must be truncated"
        );
        assert!(
            message.contains("missing field"),
            "field-level hint should survive the truncation"
        );
        assert!(
            !message.contains(&"x".repeat(600)),
            "oversized tail must not be echoed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn anthropic_error_middleware_does_not_hang_on_streaming_error_body() {
        // 防御验证：非 2xx 的流式（永不结束）错误体不得挂起中间件。
        // body 读取超时后回退通用文本，请求仍返回 Anthropic schema 错误。
        // start_paused 让超时使用虚拟时间，避免测试真实等待 5 秒。
        let app = Router::new()
            .route(
                "/v1/messages",
                get(|| async {
                    (
                        StatusCode::BAD_GATEWAY,
                        Body::from_stream(futures::stream::pending::<
                            std::result::Result<bytes::Bytes, std::convert::Infallible>,
                        >()),
                    )
                }),
            )
            .layer(from_fn(anthropic_error_response_middleware));

        let app_handle = tokio::spawn(async move {
            app.oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        });

        // 轮询推进虚拟时间：后台任务在 body 读取超时后完成回退。
        // advance 会 poll 等待时间的任务，因此无需固定 sleep 或精确时序。
        for _ in 0..100 {
            if app_handle.is_finished() {
                break;
            }
            tokio::time::advance(Duration::from_millis(100)).await;
        }

        let response = tokio::time::timeout(Duration::from_secs(1), app_handle)
            .await
            .expect("middleware must not hang on a never-ending error body")
            .expect("oneshot should succeed");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "api_error");
        assert_eq!(body["error"]["message"], "Request failed");
    }

    #[tokio::test]
    async fn anthropic_error_middleware_keeps_json_rejection_field_hint() {
        // axum 的 `Json` 提取器失败（缺失字段/类型错误）时返回纯文本 400，
        // 中间件应提取首行作为 message，让 SDK 客户端看到字段级提示。
        let app = Router::new()
            .route(
                "/v1/messages",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        "Failed to deserialize the JSON body into the target type: \
                         missing field `max_tokens` at line 1 column 21",
                    )
                }),
            )
            .layer(from_fn(anthropic_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("max_tokens"),
            "field-level hint from the JSON rejection should survive the schema wrap"
        );
        assert!(
            !body["error"]["message"].as_str().unwrap().contains("\n"),
            "only the first line of a plaintext body should be exposed"
        );
    }

    #[tokio::test]
    async fn anthropic_error_middleware_keeps_other_plaintext_errors_generic() {
        // 非反序列化错误的纯文本 body（如 500 代理错误页）不得原样回显，
        // 必须保持通用文本，避免泄露服务端内部内容。
        let app = Router::new()
            .route(
                "/v1/messages",
                get(|| async {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "panic: internal secret stack trace",
                    )
                }),
            )
            .layer(from_fn(anthropic_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "api_error");
        assert_eq!(body["error"]["message"], "Request failed");
    }

    #[tokio::test]
    async fn anthropic_error_middleware_converts_rate_limit_body() {
        // 限流中间件（rate_limit_exceeded_response）的 429 响应体是
        // `{"error": {...}}` 形式：必须提取 message 并映射到 Anthropic 的
        // rate_limit_error 类别，SDK 才能按 Anthropic schema 解析限流错误。
        let app = Router::new()
            .route(
                "/v1/messages",
                get(|| async { rate_limit_exceeded_response() }),
            )
            .layer(from_fn(anthropic_error_response_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(
            body["error"]["message"],
            "Rate limit exceeded. Please try again later."
        );
    }

    #[test]
    fn anthropic_error_type_covers_messages_specific_statuses() {
        assert_eq!(
            anthropic_error_type(StatusCode::NOT_FOUND),
            "not_found_error"
        );
        assert_eq!(
            anthropic_error_type(StatusCode::PAYLOAD_TOO_LARGE),
            "request_too_large"
        );
        assert_eq!(
            anthropic_error_type(StatusCode::from_u16(529).unwrap()),
            "overloaded_error"
        );
        assert_eq!(
            anthropic_error_type(StatusCode::UNSUPPORTED_MEDIA_TYPE),
            "invalid_request_error"
        );
    }

    #[test]
    fn test_permission_middleware_creation() {
        // 测试权限中间件可以正确创建
        let _middleware = permission_middleware(Permission::ManageProtectedUsers);
    }

    #[test]
    fn test_extract_auth_from_extensions_empty() {
        // 测试从空扩展中提取 AuthExtractor
        let req: Request<Body> = Request::new(Body::empty());
        let result = extract_auth_from_extensions(&req);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_auth_from_extensions_present() {
        // 测试从扩展中提取已注入的 AuthExtractor
        let mut req: Request<Body> = Request::new(Body::empty());
        let auth = AuthExtractor::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            CredentialKind::Jwt,
        )
        .with_permissions(vec![Permission::ManageProtectedUsers]);
        req.extensions_mut().insert(auth.clone());

        let result = extract_auth_from_extensions(&req);
        assert!(result.is_some());
        let extracted = result.unwrap();
        assert!(extracted.has_permission(&Permission::ManageProtectedUsers));
    }

    #[test]
    fn test_extract_client_ip_only_trusts_x_real_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("1.1.1.1"));
        headers.insert("x-real-ip", HeaderValue::from_static("2.2.2.2"));

        let extracted = extract_client_ip_from_headers(&headers);
        assert_eq!(extracted.as_deref(), Some("2.2.2.2"));
    }

    #[test]
    fn test_extract_client_ip_rejects_invalid_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-real-ip",
            HeaderValue::from_static("attacker-controlled-bucket"),
        );
        assert!(extract_client_ip_from_headers(&headers).is_none());
    }

    #[test]
    fn test_payment_callbacks_use_separate_rate_limit_scopes() {
        assert_eq!(
            payment_notify_scope("/api/v1/payments/notify/alipay"),
            "payment-notify-alipay"
        );
        assert_eq!(
            payment_notify_scope("/api/v1/payments/notify/wechatpay"),
            "payment-notify-wechatpay"
        );
    }

    #[tokio::test]
    async fn test_payment_notify_middleware_fails_closed_without_trusted_ip() {
        let state = AppState::with_config(AppStateConfig::default());
        let app = Router::new()
            .route(
                "/api/v1/payments/notify/alipay",
                get(|| async { StatusCode::NO_CONTENT }),
            )
            .layer(from_fn_with_state(
                state.clone(),
                payment_notify_rate_limit_middleware,
            ))
            .with_state(state);

        // 缺失 X-Real-IP：未经可信代理直连，必须 fail-closed 拒绝
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/payments/notify/alipay")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 非法 X-Real-IP（非 IP 字符串）：同样必须拒绝
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/payments/notify/alipay")
                    .header("x-real-ip", "attacker-controlled-bucket")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 可信代理注入的合法 IP：正常放行到 handler
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/payments/notify/alipay")
                    .header("x-real-ip", "203.0.113.7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[test]
    fn test_payment_callbacks_bypass_maintenance_mode_by_exact_path() {
        assert!(is_maintenance_excluded_path(
            "/api/v1/payments/notify/alipay"
        ));
        assert!(is_maintenance_excluded_path(
            "/api/v1/payments/notify/wechatpay"
        ));
        assert!(!is_maintenance_excluded_path(
            "/api/v1/payments/notify/alipay/extra"
        ));
        assert!(!is_maintenance_excluded_path("/health-check"));
    }

    #[test]
    fn maintenance_mode_allows_only_system_administrators() {
        assert!(maintenance_mode_allows_request(false, false));
        assert!(maintenance_mode_allows_request(false, true));
        assert!(!maintenance_mode_allows_request(true, false));
        assert!(maintenance_mode_allows_request(true, true));
    }

    #[test]
    fn x_api_key_only_authorized_on_messages_path() {
        // 与认证提取器的路径限制对称：只有 /v1/messages 允许 x-api-key 身份。
        assert!(x_api_key_allowed_on_path("/v1/messages"));
        assert!(!x_api_key_allowed_on_path("/v1/chat/completions"));
        assert!(!x_api_key_allowed_on_path("/api/v1/me"));
        assert!(!x_api_key_allowed_on_path("/api/v1/admin/users"));
    }

    #[tokio::test]
    async fn websocket_upgrade_header_does_not_bypass_post_rate_limiting() {
        let fixture = crate::test_support::TestIdentity::member().await;
        let state = fixture.state.clone();
        let token = fixture.token.clone();
        let app = Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(
                    |auth: AuthExtractor, State(state): State<AppState>| async move {
                        enforce_authenticated_rate_limit(&state, &auth).await?;
                        Ok::<_, ApiError>(StatusCode::NO_CONTENT)
                    },
                ),
            )
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state);

        for _ in 0..keycompute_ratelimit::DEFAULT_RPM_LIMIT {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/responses")
                        .header("Authorization", format!("Bearer {token}"))
                        .header("Upgrade", "websocket")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Upgrade", "websocket")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn x_api_key_is_not_rate_limited_outside_messages_path() {
        // 非 /v1/messages 路径携带 x-api-key：中间件直接放行，不尝试以该
        // key 身份限流（认证层随后会拒绝 x-api-key，见 extractors 测试）。
        // 连续发送超过 RPM 上限数量的请求仍全部放行，证明该路径不会以
        // x-api-key 身份消耗配额（否则会像 JWT 测试一样触发 429）。
        let state = AppState::with_config(AppStateConfig::default());
        let app = Router::new()
            .route("/other", get(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state);

        for request_number in 1..=keycompute_ratelimit::DEFAULT_RPM_LIMIT + 1 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/other")
                        .header("x-api-key", "sk-test-key")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NO_CONTENT,
                "request {request_number}: x-api-key must not consume quota outside /v1/messages"
            );
        }
    }

    #[tokio::test]
    async fn test_valid_jwt_requests_are_rate_limited() {
        let fixture = crate::test_support::TestIdentity::member().await;
        let state = fixture.state.clone();
        let token = fixture.token.clone();
        let app = Router::new()
            .route("/rate-limited", get(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
            .with_state(state);

        for request_number in 1..=keycompute_ratelimit::DEFAULT_RPM_LIMIT {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/rate-limited")
                        .header("Authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NO_CONTENT,
                "request {request_number} should remain inside the RPM allowance"
            );
        }

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/rate-limited")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        fixture.finish().await;
    }

    #[test]
    fn test_public_auth_cookie_signature_validation() {
        let secret = "super-secret";
        let cookie_value = sign_public_auth_cookie_value(secret, "identity-123");

        let valid = validate_public_auth_cookie_value(secret, &cookie_value);
        assert_eq!(valid.as_deref(), Some("identity-123"));

        let tampered = cookie_value.replacen("identity-123", "identity-456", 1);
        assert!(validate_public_auth_cookie_value(secret, &tampered).is_none());
    }

    #[tokio::test]
    async fn test_admin_auth_middleware_gates_admin_payment_routes_by_permission() {
        use keycompute_types::{PlatformRole, TenantRole};
        for role in [
            PlatformRole::None,
            PlatformRole::Operator,
            PlatformRole::Root,
        ] {
            let fixture =
                crate::test_support::TestIdentity::with_roles(role, TenantRole::Admin).await;
            let app = Router::new()
                .route(
                    "/api/v1/admin/payments/orders",
                    get(|| async { StatusCode::NO_CONTENT }),
                )
                .layer(from_fn_with_state(
                    fixture.state.clone(),
                    admin_auth_middleware,
                ))
                .with_state(fixture.state.clone());
            let request = |token: Option<&str>| {
                let mut req = Request::builder().uri("/api/v1/admin/payments/orders");
                if let Some(token) = token {
                    req = req.header("Authorization", format!("Bearer {token}"));
                }
                req.body(Body::empty()).unwrap()
            };
            assert_eq!(
                app.clone().oneshot(request(None)).await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                app.clone()
                    .oneshot(request(Some(
                        "sk-0123456789abcdef0123456789abcdef0123456789abcdef"
                    )))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            let response = app.oneshot(request(Some(&fixture.token))).await.unwrap();
            assert_eq!(
                response.status(),
                if role == PlatformRole::Root {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::FORBIDDEN
                }
            );
            fixture.finish().await;
        }
    }

    #[tokio::test]
    async fn admin_auth_middleware_reports_backend_failure_as_service_unavailable() {
        let state = AppState::with_pool(keycompute_db::DbRouter::single(
            sea_orm::DatabaseConnection::Disconnected,
        ));
        let jwt = JwtConfig::default();
        let token = JwtValidator::new(&jwt.secret, &jwt.issuer)
            .generate_identity_token(
                Uuid::new_v4(),
                Some(Uuid::new_v4()),
                0,
                Some(1),
                Some(1),
                3600,
            )
            .unwrap();
        let app = Router::new()
            .route(
                "/api/v1/admin/users",
                get(|| async { StatusCode::NO_CONTENT }),
            )
            .layer(from_fn_with_state(state.clone(), admin_auth_middleware))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/admin/users")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "authentication_unavailable");
    }
}
