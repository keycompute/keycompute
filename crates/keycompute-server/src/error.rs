//! 错误处理
//!
//! 定义 API 错误类型和响应格式

use axum::{
    Json,
    body::Body,
    http::{HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::fmt;

const UPSTREAM_REQUEST_FAILED_MESSAGE: &str = "Upstream request failed";
const UPSTREAM_REQUEST_TIMEOUT_MESSAGE: &str = "Upstream request timed out";

/// Marks responses whose client-facing message was produced by this service.
/// Middleware must use this out-of-band marker instead of trusting a JSON
/// shape that an upstream response can imitate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TrustedLocalApiError;

/// API 错误类型
#[derive(Debug)]
pub enum ApiError {
    /// 认证错误
    Auth(String),
    /// 限流错误
    RateLimit(String),
    /// 路由错误
    Routing(String),
    /// Provider 错误
    Provider(String),
    /// 配置错误
    Config(String),
    /// 验证流程错误
    Verification(String),
    /// 服务暂时不可用
    ServiceUnavailable(String),
    /// 内部错误
    Internal(String),
    /// 请求参数错误
    BadRequest(String),
    /// 资源未找到
    NotFound(String),
    /// 权限拒绝
    Forbidden(String),
    /// 节点身份不匹配
    NodeIdentityMismatch {
        expected_node_id: uuid::Uuid,
        expected_session_id: uuid::Uuid,
        actual_node_id: uuid::Uuid,
        actual_session_id: uuid::Uuid,
    },
    /// 节点任务提交冲突（409）
    NodeTaskConflict(String),
    /// 通用冲突错误（409），如并发操作冲突
    Conflict(String),
    /// Native OpenAI-compatible upstream HTTP error. The body is sanitized
    /// when converted to a public response.
    OpenAiUpstream(keycompute_types::ClientUpstreamResponse),
    /// Native Anthropic upstream HTTP error. The body is sanitized while the
    /// protocol-specific envelope and status are retained.
    AnthropicUpstream(keycompute_types::ClientUpstreamResponse),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Auth(msg) => write!(f, "Authentication error: {}", msg),
            ApiError::RateLimit(msg) => write!(f, "Rate limit error: {}", msg),
            ApiError::Routing(msg) => write!(f, "Routing error: {}", msg),
            ApiError::Provider(msg) => write!(f, "Provider error: {}", msg),
            ApiError::Config(msg) => write!(f, "Config error: {}", msg),
            ApiError::Verification(msg) => write!(f, "Verification error: {}", msg),
            ApiError::ServiceUnavailable(msg) => write!(f, "Service unavailable: {}", msg),
            ApiError::Internal(msg) => write!(f, "Internal error: {}", msg),
            ApiError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            ApiError::NotFound(msg) => write!(f, "Not found: {}", msg),
            ApiError::Forbidden(msg) => write!(f, "Forbidden: {}", msg),
            ApiError::NodeIdentityMismatch { .. } => write!(f, "Node identity mismatch"),
            ApiError::NodeTaskConflict(msg) => write!(f, "Node task conflict: {}", msg),
            ApiError::Conflict(msg) => write!(f, "Conflict: {}", msg),
            ApiError::OpenAiUpstream(response) => {
                write!(f, "OpenAI upstream HTTP error: {}", response.status)
            }
            ApiError::AnthropicUpstream(response) => {
                write!(f, "Anthropic upstream HTTP error: {}", response.status)
            }
        }
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let ApiError::OpenAiUpstream(upstream) = &self {
            return native_openai_error_response(upstream);
        }
        if let ApiError::AnthropicUpstream(upstream) = &self {
            return native_anthropic_error_response(upstream);
        }
        let (status, error_message) = match &self {
            ApiError::Auth(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
            ApiError::RateLimit(msg) => (StatusCode::TOO_MANY_REQUESTS, msg.clone()),
            ApiError::Routing(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg.clone()),
            ApiError::Provider(msg) => (StatusCode::BAD_GATEWAY, msg.clone()),
            ApiError::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            ApiError::Verification(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg.clone()),
            ApiError::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg.clone()),
            ApiError::Internal(msg) => {
                tracing::error!(error = %msg, "Internal API error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            ApiError::NodeIdentityMismatch { .. } => (
                StatusCode::UNAUTHORIZED,
                "Node identity mismatch: node_id or session_id does not match authenticated session"
                    .to_string(),
            ),
            ApiError::NodeTaskConflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            ApiError::OpenAiUpstream(_) | ApiError::AnthropicUpstream(_) => unreachable!(),
        };

        let body = Json(json!({
            "error": {
                "message": error_message,
                "type": error_type(&self),
                "code": status.as_u16(),
            }
        }));

        let mut response = (status, body).into_response();
        response.extensions_mut().insert(TrustedLocalApiError);
        response
    }
}

fn error_type(error: &ApiError) -> &'static str {
    match error {
        ApiError::Auth(_) => "authentication_error",
        ApiError::RateLimit(_) => "rate_limit_error",
        ApiError::Routing(_) => "routing_error",
        ApiError::Provider(_) => "provider_error",
        ApiError::Config(_) => "config_error",
        ApiError::Verification(_) => "verification_error",
        ApiError::ServiceUnavailable(_) => "service_unavailable_error",
        ApiError::Internal(_) => "internal_error",
        ApiError::BadRequest(_) => "bad_request_error",
        ApiError::NotFound(_) => "not_found_error",
        ApiError::Forbidden(_) => "forbidden_error",
        ApiError::NodeIdentityMismatch { .. } => "node_identity_mismatch_error",
        ApiError::NodeTaskConflict(_) => "node_task_conflict_error",
        ApiError::Conflict(_) => "conflict_error",
        ApiError::OpenAiUpstream(_) | ApiError::AnthropicUpstream(_) => "upstream_error",
    }
}

fn native_error_status(status: u16) -> StatusCode {
    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY)
}

fn append_safe_upstream_error_headers(
    mut builder: axum::http::response::Builder,
    headers: &[(String, String)],
) -> axum::http::response::Builder {
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        let allowed = matches!(
            lower.as_str(),
            "request-id"
                | "x-request-id"
                | "x-amzn-requestid"
                | "retry-after"
                | "retry-after-ms"
                | "openai-processing-ms"
        ) || lower.starts_with("x-ratelimit-")
            || lower.starts_with("anthropic-ratelimit-");
        if !allowed {
            continue;
        }
        let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value))
        else {
            continue;
        };
        builder = builder.header(name, value);
    }
    builder
}

fn openai_error_defaults(status: StatusCode) -> (&'static str, Value) {
    match status.as_u16() {
        401 => ("authentication_error", json!("invalid_api_key")),
        403 => ("permission_error", Value::Null),
        404 => ("not_found_error", Value::Null),
        413 => ("invalid_request_error", json!("request_too_large")),
        429 => ("rate_limit_error", json!("rate_limit_exceeded")),
        500..=599 => ("server_error", json!("server_error")),
        _ => ("invalid_request_error", Value::Null),
    }
}

fn native_openai_error_response(upstream: &keycompute_types::ClientUpstreamResponse) -> Response {
    let status = native_error_status(upstream.status);
    let parsed = serde_json::from_str::<Value>(&upstream.body).unwrap_or(Value::Null);
    let error = parsed.get("error").and_then(Value::as_object);
    let (fallback_type, fallback_code) = openai_error_defaults(status);
    let error_type = crate::middleware::sanitize_openai_responses_error_type(
        error.and_then(|value| value.get("type")),
        fallback_type,
    );
    let code = crate::middleware::sanitize_openai_responses_error_code(
        error.and_then(|value| value.get("code")),
        fallback_code,
    );
    let param = crate::middleware::sanitize_openai_responses_error_param(
        error.and_then(|value| value.get("param")),
    );
    let body = json!({
        "error": {
            "message": UPSTREAM_REQUEST_FAILED_MESSAGE,
            "type": error_type,
            "param": param,
            "code": code,
        }
    })
    .to_string();
    append_safe_upstream_error_headers(Response::builder().status(status), &upstream.headers)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn anthropic_error_type_for_status(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 405 | 415 | 422 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        503 | 529 => "overloaded_error",
        _ => "api_error",
    }
}

fn sanitized_anthropic_error_type(body: &Value, status: StatusCode) -> String {
    const ALLOWED: &[&str] = &[
        "api_error",
        "authentication_error",
        "billing_error",
        "gateway_timeout",
        "invalid_request_error",
        "not_found_error",
        "overloaded_error",
        "permission_error",
        "rate_limit_error",
        "request_too_large",
    ];
    body.pointer("/error/type")
        .and_then(Value::as_str)
        .filter(|value| ALLOWED.contains(value))
        .unwrap_or_else(|| anthropic_error_type_for_status(status))
        .to_string()
}

fn sanitized_upstream_request_id(body: &Value) -> Option<&str> {
    body.get("request_id")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        })
}

fn native_anthropic_error_response(
    upstream: &keycompute_types::ClientUpstreamResponse,
) -> Response {
    let status = native_error_status(upstream.status);
    let parsed = serde_json::from_str::<Value>(&upstream.body).unwrap_or(Value::Null);
    let error_type = sanitized_anthropic_error_type(&parsed, status);
    let mut body = json!({
        "type": "error",
        "error": {
            "type": error_type,
            "message": UPSTREAM_REQUEST_FAILED_MESSAGE,
        }
    });
    if let Some(request_id) = sanitized_upstream_request_id(&parsed) {
        body["request_id"] = Value::String(request_id.to_string());
    }
    append_safe_upstream_error_headers(Response::builder().status(status), &upstream.headers)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// API 结果类型
pub type Result<T> = std::result::Result<T, ApiError>;

/// 从 anyhow 错误转换
impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        ApiError::Internal(err.to_string())
    }
}

/// 从 sea-orm 错误转换
impl From<sea_orm::DbErr> for ApiError {
    fn from(err: sea_orm::DbErr) -> Self {
        ApiError::Internal(err.to_string())
    }
}

/// 从数据库错误转换
impl From<keycompute_db::DbError> for ApiError {
    fn from(err: keycompute_db::DbError) -> Self {
        match err {
            keycompute_db::DbError::NotFound { entity, id } => {
                ApiError::NotFound(format!("{} not found: {}", entity, id))
            }
            keycompute_db::DbError::DatabaseError(_) => ApiError::Internal(err.to_string()),
            keycompute_db::DbError::Other(msg) => {
                // 节点任务提交相关的冲突错误应该返回 409
                match msg.as_str() {
                    "duplicate_submission_session_mismatch"
                    | "duplicate_submission_conflict"
                    | "grace_period_expired"
                    | "invalid_task_state"
                    | "lease_mismatch"
                    | "node_result_type_mismatch"
                    | "task_expired_during_complete" => ApiError::NodeTaskConflict(msg),
                    _ => ApiError::Internal(msg),
                }
            }
            _ => ApiError::Internal(err.to_string()),
        }
    }
}

/// 从 node-gateway 执行错误映射: client_error → 400, 其他 → 500 (B2 修复)
impl From<node_gateway::NodeExecutionError> for ApiError {
    fn from(err: node_gateway::NodeExecutionError) -> Self {
        use node_gateway::NodeExecutionError as NE;
        match err {
            NE::ClientError { code, message } => {
                ApiError::BadRequest(format!("{}: {}", code, message))
            }
            NE::Other { source, .. } => ApiError::Internal(source.to_string()),
        }
    }
}

/// 从 keycompute-types 错误转换
impl From<keycompute_types::KeyComputeError> for ApiError {
    fn from(err: keycompute_types::KeyComputeError) -> Self {
        use keycompute_types::KeyComputeError;
        match err {
            // 认证与授权
            KeyComputeError::AuthError(msg) => ApiError::Auth(msg),
            KeyComputeError::PermissionDenied(msg) => ApiError::Forbidden(msg),
            KeyComputeError::VerificationError(msg) => ApiError::Verification(msg),

            // 限流
            KeyComputeError::RateLimitExceeded(msg) => ApiError::RateLimit(msg),

            // 路由
            // 注意：route() 的两个入口（chat_completions / messages）改用
            // map_routing_error 做更细粒度映射（RoutingFailed→404、
            // NoReadyNode→503）。此通用转换保留 503（ApiError::Routing）：
            // gateway executor 在执行阶段（所有 target 失败）也会产生
            // RoutingFailed，其语义是容量/上游故障而非模型不可用，503 更合适。
            // 执行期错误由 map_execution_error 映射（RoutingFailed→503，
            // 其余保持 Internal 屏蔽细节），不经过此分支；此分支主要服务于
            // 未显式使用 map_* 的调用点。
            KeyComputeError::RoutingFailed(msg) => ApiError::Routing(msg),
            KeyComputeError::NoReadyNode(msg) => ApiError::Routing(msg),

            // Provider/transport 错误可能携带上游响应正文、endpoint 或底层网络
            // 细节。通用转换必须是安全边界；原始信息只留在内部日志/追踪中。
            KeyComputeError::ProviderError(_) | KeyComputeError::UpstreamFailure { .. } => {
                ApiError::Provider(UPSTREAM_REQUEST_FAILED_MESSAGE.to_string())
            }
            KeyComputeError::ProviderTimeout(_, _) => {
                ApiError::Provider(UPSTREAM_REQUEST_TIMEOUT_MESSAGE.to_string())
            }

            // 数据库
            KeyComputeError::DatabaseError(msg) => ApiError::Internal(msg),

            // 配置
            KeyComputeError::ConfigError(msg) => ApiError::Config(msg),
            KeyComputeError::ServiceUnavailable(msg) => ApiError::ServiceUnavailable(msg),

            // 验证与请求
            KeyComputeError::ValidationError(msg) => ApiError::BadRequest(msg),
            KeyComputeError::InvalidRequest(msg) => ApiError::BadRequest(msg),

            // 未找到
            KeyComputeError::NotFound(msg) => ApiError::NotFound(msg),

            // 网络
            KeyComputeError::NetworkError(_) => {
                ApiError::Provider(UPSTREAM_REQUEST_FAILED_MESSAGE.to_string())
            }
            KeyComputeError::Timeout(_) => {
                ApiError::Provider(UPSTREAM_REQUEST_TIMEOUT_MESSAGE.to_string())
            }

            // 内部错误
            KeyComputeError::Internal(msg) => ApiError::Internal(msg),
            KeyComputeError::SerializationError(msg) => ApiError::Internal(msg),
        }
    }
}

/// 将路由错误映射为对外 API 错误（供 chat_completions / messages 入口使用）。
///
/// 区分客户端可修复与瞬时容量问题：
/// - `RoutingFailed`：本协议下无可用账号，请求的模型在此接口不可用（404）
/// - `NoReadyNode`：node 模型暂无在线节点，容量/可用性问题（503，客户端可重试）
/// - 其他（数据库等内部故障）：500，不泄露内部细节
pub fn map_routing_error(e: keycompute_types::KeyComputeError, protocol: &str) -> ApiError {
    use keycompute_types::KeyComputeError;
    match e {
        KeyComputeError::RoutingFailed(model) => ApiError::NotFound(format!(
            "Model '{model}' is not available on the {protocol}-compatible endpoint. \
             Check that the model name is correct and an enabled {protocol} channel account declares it. \
             If the account pool is temporarily unavailable, retry later."
        )),
        KeyComputeError::NoReadyNode(model) => ApiError::ServiceUnavailable(format!(
            "Model 'node:{model}' is temporarily unavailable: no ready node is online. \
             Please retry later."
        )),
        // route() 当前只产生 RoutingFailed / NoReadyNode / DatabaseError /
        // Internal，其余分支为防御性兜底：数据库与内部故障统一 500，
        // 响应体不泄露具体原因（ApiError::Internal 输出通用消息）
        other => ApiError::Internal(format!("Routing failed: {other}")),
    }
}

/// 将执行阶段（gateway executor）的错误映射为对外 API 错误
/// （供 chat_completions / messages 入口的 Execution failed 包装使用）。
///
/// 执行阶段所有 target 失败且无具体错误时产生 `RoutingFailed`（executor
/// 的 last_error 为 None 兜底），语义是容量/上游故障而非模型不可用，映射
/// 为 503（客户端可重试）；其余执行期错误（ProviderError 等）可能携带上游
/// 内部细节，继续由 `ApiError::Internal` 屏蔽为通用 500，不向客户端泄露。
pub fn map_execution_error(e: keycompute_types::KeyComputeError) -> ApiError {
    use keycompute_types::KeyComputeError;
    match e {
        KeyComputeError::RoutingFailed(model) => ApiError::ServiceUnavailable(format!(
            "Model '{model}' could not be completed: all configured upstream targets failed. \
             Please retry later."
        )),
        other => ApiError::Internal(format!("Execution failed: {other}")),
    }
}

/// Map an exhausted native-protocol execution while preserving a bounded
/// upstream HTTP response captured by the transport. If a custom transport did
/// not retain the body, the structured status still survives in a sanitized
/// protocol envelope.
pub fn map_openai_execution_error(
    error: keycompute_types::KeyComputeError,
    upstream: Option<keycompute_types::ClientUpstreamResponse>,
) -> ApiError {
    if let Some(upstream) = upstream {
        return ApiError::OpenAiUpstream(upstream);
    }
    if let keycompute_types::KeyComputeError::UpstreamFailure {
        status: Some(status),
        ..
    } = &error
    {
        return ApiError::OpenAiUpstream(keycompute_types::ClientUpstreamResponse {
            status: *status,
            headers: Vec::new(),
            body: String::new(),
        });
    }
    map_execution_error(error)
}

pub fn map_anthropic_execution_error(
    error: keycompute_types::KeyComputeError,
    upstream: Option<keycompute_types::ClientUpstreamResponse>,
) -> ApiError {
    if let Some(upstream) = upstream {
        return ApiError::AnthropicUpstream(upstream);
    }
    if let keycompute_types::KeyComputeError::UpstreamFailure {
        status: Some(status),
        ..
    } = &error
    {
        return ApiError::AnthropicUpstream(keycompute_types::ClientUpstreamResponse {
            status: *status,
            headers: Vec::new(),
            body: String::new(),
        });
    }
    map_execution_error(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_error_display() {
        let err = ApiError::Auth("Invalid key".to_string());
        assert!(err.to_string().contains("Authentication error"));
    }

    #[test]
    fn test_error_type() {
        assert_eq!(
            error_type(&ApiError::Auth("test".to_string())),
            "authentication_error"
        );
        assert_eq!(
            error_type(&ApiError::RateLimit("test".to_string())),
            "rate_limit_error"
        );
    }

    #[tokio::test]
    async fn generic_rate_limit_error_keeps_rest_contract() {
        let response = ApiError::RateLimit("slow down".to_string()).into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["message"], "slow down");
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], 429);
        assert!(body["error"].get("param").is_none());
    }

    #[test]
    fn mismatched_node_result_is_a_client_visible_task_conflict() {
        let error = ApiError::from(keycompute_db::DbError::Other(
            "node_result_type_mismatch".to_string(),
        ));

        assert!(matches!(error, ApiError::NodeTaskConflict(_)));
        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn internal_error_response_hides_details() {
        let response =
            ApiError::Internal("duplicate key violates constraint secret_table_key".to_string())
                .into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains("Internal server error"));
        assert!(!body.contains("secret_table_key"));
        assert!(!body.contains("duplicate key"));
    }

    #[tokio::test]
    async fn generic_provider_error_responses_hide_upstream_details() {
        let errors = [
            keycompute_types::KeyComputeError::ProviderError(
                "provider returned secret-token-123".to_string(),
            ),
            keycompute_types::KeyComputeError::UpstreamFailure {
                status: Some(502),
                stable_code: "upstream_http_502".to_string(),
                retryable: true,
                summary: "raw upstream body with secret-token-123".to_string(),
            },
            keycompute_types::KeyComputeError::NetworkError(
                "https://internal.example/secret-token-123".to_string(),
            ),
        ];

        for error in errors {
            let response = ApiError::from(error).into_response();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body = String::from_utf8(body.to_vec()).unwrap();
            assert!(body.contains(UPSTREAM_REQUEST_FAILED_MESSAGE));
            assert!(!body.contains("secret-token-123"));
            assert!(!body.contains("internal.example"));
        }
    }

    #[tokio::test]
    async fn map_routing_error_not_found_body_is_client_safe() {
        let err = map_routing_error(
            keycompute_types::KeyComputeError::RoutingFailed("deepseek-v4-flash".to_string()),
            "openai",
        );
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        // 标准错误格式；消息仅含模型名与协议名（客户端可修复信息），
        // 不泄露内部实现细节（如 routing failed 前缀）
        assert!(body.contains("not_found_error"));
        assert!(body.contains("deepseek-v4-flash"));
        assert!(!body.contains("routing failed:"));
    }

    #[test]
    fn map_routing_error_maps_no_account_to_not_found() {
        let err = map_routing_error(
            keycompute_types::KeyComputeError::RoutingFailed("deepseek-v4-flash".to_string()),
            "openai",
        );
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn map_routing_error_message_names_the_entry_protocol() {
        // 消息中的协议名由入口参数注入（anthropic.rs 传 "anthropic"），
        // 防止后续误传其他值导致客户端收到错误的协议提示
        let err = map_routing_error(
            keycompute_types::KeyComputeError::RoutingFailed("claude-x".to_string()),
            "anthropic",
        );
        assert!(err.to_string().contains("anthropic-compatible endpoint"));
        let err = map_routing_error(
            keycompute_types::KeyComputeError::RoutingFailed("gpt-5".to_string()),
            "openai",
        );
        assert!(err.to_string().contains("openai-compatible endpoint"));
    }

    #[test]
    fn map_routing_error_maps_no_ready_node_to_service_unavailable() {
        let err = map_routing_error(
            keycompute_types::KeyComputeError::NoReadyNode("llama3".to_string()),
            "openai",
        );
        // 容量问题提示可重试，且模型名保留 node: 前缀便于客户端对应
        assert!(err.to_string().contains("node:llama3"));
        assert!(err.to_string().contains("retry"));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn map_routing_error_keeps_internal_errors_as_500() {
        let err = map_routing_error(
            keycompute_types::KeyComputeError::DatabaseError("connection refused".to_string()),
            "openai",
        );
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn map_execution_error_maps_routing_failed_to_service_unavailable() {
        // 执行期所有 target 失败：容量/上游故障语义，503 可重试
        let err = map_execution_error(keycompute_types::KeyComputeError::RoutingFailed(
            "gpt-4o".to_string(),
        ));
        assert!(err.to_string().contains("gpt-4o"));
        assert!(err.to_string().contains("retry"));
        // 消息不泄露内部细节（如 execution failed 前缀）
        assert!(!err.to_string().contains("Execution failed"));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn map_execution_error_keeps_provider_errors_hidden_as_500() {
        // ProviderError 可能携带上游内部细节，保持 Internal 屏蔽为通用 500
        let err = map_execution_error(keycompute_types::KeyComputeError::ProviderError(
            "upstream secret detail".to_string(),
        ));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Internal server error"));
        assert!(!body.contains("upstream secret detail"));
    }

    #[tokio::test]
    async fn native_openai_error_preserves_status_classification_and_request_id() {
        let response = ApiError::OpenAiUpstream(keycompute_types::ClientUpstreamResponse {
            status: 429,
            headers: vec![
                ("x-request-id".to_string(), "req_upstream_123".to_string()),
                ("retry-after".to_string(), "7".to_string()),
                ("set-cookie".to_string(), "secret=cookie".to_string()),
            ],
            body: json!({
                "error": {
                    "message": "account sk-secret has exhausted quota",
                    "type": "rate_limit_error",
                    "code": "rate_limit_exceeded",
                    "param": "model",
                    "provider_private": "do not expose"
                }
            })
            .to_string(),
        })
        .into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["x-request-id"], "req_upstream_123");
        assert_eq!(response.headers()["retry-after"], "7");
        assert!(response.headers().get("set-cookie").is_none());
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["message"], UPSTREAM_REQUEST_FAILED_MESSAGE);
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert_eq!(body["error"]["param"], "model");
        assert!(body["error"].get("provider_private").is_none());
        assert!(!body.to_string().contains("sk-secret"));
    }

    #[tokio::test]
    async fn native_anthropic_error_preserves_schema_and_safe_request_id() {
        let response = ApiError::AnthropicUpstream(keycompute_types::ClientUpstreamResponse {
            status: 529,
            headers: vec![("request-id".to_string(), "req-header-1".to_string())],
            body: json!({
                "type": "error",
                "error": {
                    "type": "overloaded_error",
                    "message": "internal provider detail"
                },
                "request_id": "req_body-1",
                "debug": "secret"
            })
            .to_string(),
        })
        .into_response();

        assert_eq!(response.status().as_u16(), 529);
        assert_eq!(response.headers()["request-id"], "req-header-1");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "overloaded_error");
        assert_eq!(body["error"]["message"], UPSTREAM_REQUEST_FAILED_MESSAGE);
        assert_eq!(body["request_id"], "req_body-1");
        assert!(body.get("debug").is_none());
        assert!(!body.to_string().contains("internal provider detail"));
    }
}
