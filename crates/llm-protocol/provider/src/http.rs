//! HTTP 传输层抽象
//!
//! 定义统一的 HTTP 客户端接口，供 Provider Adapter 使用。
//! 具体实现由 llm-gateway 提供，避免循环依赖。

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt};
use keycompute_types::Result;
use serde::{Deserialize, Serialize};
use std::ops::Deref;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamResponseMeta {
    pub status: u16,
    pub headers_received_at: DateTime<Utc>,
    pub upstream_request_id: Option<String>,
    /// Kept for protocol adapters to select provider-specific correlation headers.
    pub headers: Vec<(String, String)>,
}
impl UpstreamResponseMeta {
    pub fn synthetic_success() -> Self {
        Self {
            status: 200,
            headers_received_at: Utc::now(),
            upstream_request_id: None,
            headers: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct UpstreamResponse<T> {
    pub meta: UpstreamResponseMeta,
    pub body: T,
}
impl<T> UpstreamResponse<T> {
    pub fn map_body<U>(self, map: impl FnOnce(T) -> U) -> UpstreamResponse<U> {
        UpstreamResponse {
            meta: self.meta,
            body: map(self.body),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamFailureKind {
    Transport,
    Timeout,
    HttpStatus,
    BodyRead,
    Protocol,
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{stable_error_code}: {sanitized_summary}")]
pub struct UpstreamFailure {
    pub kind: UpstreamFailureKind,
    pub status: Option<u16>,
    pub headers_received_at: Option<DateTime<Utc>>,
    pub upstream_request_id: Option<String>,
    /// Bounded native-protocol HTTP error retained for the public compatibility
    /// handler. Debug output of the nested value redacts its body.
    #[serde(skip)]
    pub client_response: Option<Box<keycompute_types::ClientUpstreamResponse>>,
    pub retryable: bool,
    pub stable_error_code: String,
    pub sanitized_summary: String,
}

impl UpstreamFailure {
    fn transport(error: &reqwest::Error, request_is_idempotent: bool) -> Self {
        let timeout = error.is_timeout();
        let definitely_pre_dispatch = error.is_connect();
        let ambiguous_after_dispatch = !request_is_idempotent && !definitely_pre_dispatch;
        Self {
            kind: if timeout {
                UpstreamFailureKind::Timeout
            } else {
                UpstreamFailureKind::Transport
            },
            status: None,
            headers_received_at: None,
            upstream_request_id: None,
            client_response: None,
            retryable: definitely_pre_dispatch || (request_is_idempotent && timeout),
            stable_error_code: if ambiguous_after_dispatch && timeout {
                "upstream_ambiguous_timeout"
            } else if ambiguous_after_dispatch {
                "upstream_ambiguous_transport"
            } else if timeout {
                "upstream_timeout"
            } else {
                "upstream_transport"
            }
            .to_string(),
            sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
        }
    }

    pub fn into_keycompute_error(self) -> keycompute_types::KeyComputeError {
        keycompute_types::KeyComputeError::UpstreamFailure {
            status: self.status,
            stable_code: self.stable_error_code,
            retryable: self.retryable,
            summary: self.sanitized_summary,
        }
    }
}

fn response_meta(response: &reqwest::Response) -> UpstreamResponseMeta {
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect::<Vec<_>>();
    let upstream_request_id = ["x-request-id", "request-id", "x-amzn-requestid"]
        .iter()
        .find_map(|name| response.headers().get(*name))
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(128).collect());
    UpstreamResponseMeta {
        status: response.status().as_u16(),
        headers_received_at: Utc::now(),
        upstream_request_id,
        headers,
    }
}

const MAX_HTTP_FAILURE_INSPECTION_BYTES: usize = 8 * 1024;

/// Maximum decoded body retained for JSON passthrough responses. Responses
/// requests can legitimately contain large inline skill payloads, but an
/// upstream must not be able to make the gateway buffer an unbounded body.
pub const MAX_JSON_PASSTHROUGH_BODY_BYTES: usize = 96 * 1024 * 1024;
/// Maximum estimated live memory while a passthrough JSON body and its parsed
/// `serde_json::Value` coexist. A mostly-string 96 MiB response remains valid,
/// while high-cardinality arrays/objects are rejected before tree allocation.
pub const MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES: usize = 224 * 1024 * 1024;
/// Parsed JSON whose estimated text-plus-tree working set exceeds this value
/// must retain a process-wide large-body permit even when its wire body is
/// small. This closes the many-tiny-values bypass of the raw-byte threshold.
pub const LARGE_JSON_WORKING_SET_ADMISSION_BYTES: usize = 16 * 1024 * 1024;
/// Bodies below this threshold use the ordinary fast path. Larger bodies and
/// responses without a trustworthy decoded length share a process-wide budget.
pub const LARGE_JSON_BODY_ADMISSION_BYTES: usize = 4 * 1024 * 1024;
const LARGE_JSON_BODY_CONCURRENCY: usize = 2;

/// Conservatively estimate the bytes simultaneously retained while JSON text
/// is deserialized into a `serde_json::Value`. Structural bytes are counted
/// only outside strings so large base64/string payloads retain their documented
/// allowance, while arrays or maps containing many tiny values pay for their
/// substantially larger tree representation.
pub fn estimated_json_parse_working_set_bytes(text: &[u8]) -> usize {
    let mut in_string = false;
    let mut escaped = false;
    let mut structural_overhead = 0usize;
    for byte in text {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
        } else if *byte == b'"' {
            in_string = true;
        } else if matches!(*byte, b'{' | b'[' | b',' | b':') {
            structural_overhead = structural_overhead.saturating_add(64);
        }
    }

    // The input text and parsed strings can coexist during deserialization.
    text.len()
        .saturating_mul(2)
        .saturating_add(structural_overhead)
}

fn large_json_body_slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(Semaphore::new(LARGE_JSON_BODY_CONCURRENCY))))
}

/// Cloneable ownership of one process-wide large-response slot. Native events
/// carry this guard until their resident JSON/SSE bytes leave the gateway.
#[derive(Debug, Clone)]
pub struct LargeBodyPermit {
    _permit: Arc<OwnedSemaphorePermit>,
}

/// Attempt to reserve a process-wide large-response slot without joining an
/// unbounded waiter queue. Parsers use this after retaining their small-body
/// allowance so overload cannot multiply that retained memory by the number of
/// concurrent requests.
pub fn try_acquire_large_body_permit() -> Option<LargeBodyPermit> {
    let permit = large_json_body_slots().try_acquire_owned().ok()?;
    Some(LargeBodyPermit {
        _permit: Arc::new(permit),
    })
}

/// A collected response body paired with the admission slot protecting its
/// memory. The permit can be moved into the parsed native event.
#[derive(Debug)]
pub struct AdmittedResponseText {
    text: String,
    permit: Option<LargeBodyPermit>,
}

impl AdmittedResponseText {
    pub fn unadmitted(text: String) -> Self {
        Self { text, permit: None }
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn into_parts(self) -> (String, Option<LargeBodyPermit>) {
        (self.text, self.permit)
    }

    pub fn into_string(self) -> String {
        self.text
    }
}

impl Deref for AdmittedResponseText {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

impl From<String> for AdmittedResponseText {
    fn from(text: String) -> Self {
        Self::unadmitted(text)
    }
}

/// Maximum decoded body retained for non-success JSON passthrough responses.
/// Error payloads only need to preserve provider diagnostics and must not be
/// able to consume the much larger success-response allowance.
pub const MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES: usize = 1024 * 1024;

pub const fn json_passthrough_body_limit(status: u16) -> usize {
    if status >= 200 && status < 300 {
        MAX_JSON_PASSTHROUGH_BODY_BYTES
    } else {
        MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES
    }
}

pub const fn http_status_is_retryable(status: u16) -> bool {
    status == 408 || status == 409 || status == 429 || status >= 500
}

pub fn body_read_failure(
    meta: &UpstreamResponseMeta,
    stable_error_code: &str,
    summary: impl Into<String>,
) -> UpstreamFailure {
    UpstreamFailure {
        kind: UpstreamFailureKind::BodyRead,
        status: Some(meta.status),
        headers_received_at: Some(meta.headers_received_at),
        upstream_request_id: meta.upstream_request_id.clone(),
        client_response: None,
        retryable: http_status_is_retryable(meta.status),
        stable_error_code: stable_error_code.to_string(),
        sanitized_summary: keycompute_types::sanitize_error_summary(&summary.into()),
    }
}

fn append_bounded_body(
    body: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
    meta: &UpstreamResponseMeta,
) -> std::result::Result<(), UpstreamFailure> {
    if chunk.len() > max_bytes.saturating_sub(body.len()) {
        return Err(body_read_failure(
            meta,
            "upstream_body_too_large",
            format!("Upstream response body exceeds the {max_bytes}-byte limit"),
        ));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn try_admit_growing_large_body<T>(
    potentially_large: bool,
    retained_bytes: usize,
    incoming_bytes: usize,
    permit: &mut Option<T>,
    try_acquire: impl FnOnce() -> Option<T>,
    meta: &UpstreamResponseMeta,
) -> std::result::Result<(), UpstreamFailure> {
    if potentially_large
        && permit.is_none()
        && incoming_bytes > LARGE_JSON_BODY_ADMISSION_BYTES.saturating_sub(retained_bytes)
    {
        *permit = Some(try_acquire().ok_or_else(|| {
            body_read_failure(
                meta,
                "upstream_body_capacity_exhausted",
                "Upstream response body capacity is exhausted",
            )
        })?);
    }
    Ok(())
}

fn try_admit_declared_or_unknown_large_body<T>(
    potentially_large: bool,
    content_length: Option<u64>,
    try_acquire: impl FnOnce() -> Option<T>,
    meta: &UpstreamResponseMeta,
) -> std::result::Result<Option<T>, UpstreamFailure> {
    if potentially_large
        && content_length.is_none_or(|length| length > LARGE_JSON_BODY_ADMISSION_BYTES as u64)
    {
        return try_acquire().map(Some).ok_or_else(|| {
            body_read_failure(
                meta,
                "upstream_body_capacity_exhausted",
                "Upstream response body capacity is exhausted",
            )
        });
    }
    Ok(None)
}

fn finish_bounded_response_text(
    body: Vec<u8>,
    permit: Option<LargeBodyPermit>,
    meta: &UpstreamResponseMeta,
) -> std::result::Result<AdmittedResponseText, UpstreamFailure> {
    let text = String::from_utf8(body).map_err(|_| {
        body_read_failure(
            meta,
            "upstream_body_invalid_utf8",
            "Upstream response body is not valid UTF-8",
        )
    })?;
    Ok(AdmittedResponseText { text, permit })
}

/// Collect a reqwest response with a decoded-byte limit while preserving the
/// response metadata already captured by the caller.
pub async fn collect_bounded_response_text(
    response: reqwest::Response,
    meta: &UpstreamResponseMeta,
    max_bytes: usize,
) -> std::result::Result<AdmittedResponseText, UpstreamFailure> {
    let content_length = response.content_length();
    if content_length.is_some_and(|length| length > max_bytes as u64) {
        return Err(body_read_failure(
            meta,
            "upstream_body_too_large",
            format!("Upstream response body exceeds the {max_bytes}-byte limit"),
        ));
    }

    let potentially_large = max_bytes > LARGE_JSON_BODY_ADMISSION_BYTES;
    let mut permit = try_admit_declared_or_unknown_large_body(
        potentially_large,
        content_length,
        try_acquire_large_body_permit,
        meta,
    )?;
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|error| body_read_failure(meta, "upstream_body_read", error.to_string()))?;
        try_admit_growing_large_body(
            potentially_large,
            body.len(),
            chunk.len(),
            &mut permit,
            try_acquire_large_body_permit,
            meta,
        )?;
        append_bounded_body(&mut body, &chunk, max_bytes, meta)?;
    }
    finish_bounded_response_text(body, permit, meta)
}

fn summarize_http_failure_body(status: u16, body: &[u8]) -> String {
    // The upstream body is untrusted and may contain credentials, request
    // fragments, or provider-internal details. Only retain the one allowlisted
    // signal needed for the OpenAI stream_options compatibility retry.
    let mentions_stream_options = matches!(status, 400 | 422)
        && String::from_utf8_lossy(body)
            .to_ascii_lowercase()
            .contains("stream_options");
    if mentions_stream_options {
        "Upstream rejected stream_options".to_string()
    } else {
        format!("Upstream returned HTTP {status}")
    }
}

/// Consume a bounded prefix of an unsuccessful HTTP response and return an
/// allowlisted summary. Raw upstream bodies must never enter errors, traces,
/// logs, or admin monitoring records.
pub async fn capture_http_failure_response(mut response: reqwest::Response) -> (String, String) {
    let status = response.status().as_u16();
    let mut inspected = Vec::new();
    while inspected.len() < MAX_HTTP_FAILURE_INSPECTION_BYTES {
        let Ok(Some(chunk)) = response.chunk().await else {
            break;
        };
        let remaining = MAX_HTTP_FAILURE_INSPECTION_BYTES - inspected.len();
        inspected.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if chunk.len() >= remaining {
            break;
        }
    }
    let summary = summarize_http_failure_body(status, &inspected);
    let body = String::from_utf8_lossy(&inspected).into_owned();
    (summary, body)
}

pub async fn summarize_http_failure_response(response: reqwest::Response) -> String {
    capture_http_failure_response(response).await.0
}

async fn http_failure(response: reqwest::Response, meta: UpstreamResponseMeta) -> UpstreamFailure {
    let status = meta.status;
    let (summary, body) = capture_http_failure_response(response).await;
    UpstreamFailure {
        kind: UpstreamFailureKind::HttpStatus,
        status: Some(status),
        headers_received_at: Some(meta.headers_received_at),
        upstream_request_id: meta.upstream_request_id.clone(),
        client_response: Some(Box::new(keycompute_types::ClientUpstreamResponse {
            status,
            headers: meta.headers,
            body,
        })),
        retryable: http_status_is_retryable(status),
        stable_error_code: format!("upstream_http_{status}"),
        sanitized_summary: keycompute_types::sanitize_error_summary(&summary),
    }
}

/// HTTP 传输层 trait
///
/// 抽象 HTTP 客户端操作，支持：
/// - 普通请求
/// - 流式请求
/// - multipart/form-data 请求
/// - 超时控制
#[async_trait]
pub trait HttpTransport: Send + Sync + std::fmt::Debug {
    /// Structured response variant. Implementations should override this to preserve metadata.
    async fn post_json_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> std::result::Result<UpstreamResponse<String>, UpstreamFailure> {
        self.post_json(url, headers, body)
            .await
            .map(|body| UpstreamResponse {
                meta: UpstreamResponseMeta {
                    status: 200,
                    headers_received_at: Utc::now(),
                    upstream_request_id: None,
                    headers: Vec::new(),
                },
                body,
            })
            .map_err(|error| UpstreamFailure {
                kind: UpstreamFailureKind::Transport,
                status: None,
                headers_received_at: None,
                upstream_request_id: None,
                client_response: None,
                retryable: error.is_retryable(),
                stable_error_code: "upstream_transport".to_string(),
                sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
            })
    }

    async fn post_stream_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> std::result::Result<UpstreamResponse<ByteStream>, UpstreamFailure> {
        self.post_stream(url, headers, body)
            .await
            .map(|body| UpstreamResponse {
                meta: UpstreamResponseMeta {
                    status: 200,
                    headers_received_at: Utc::now(),
                    upstream_request_id: None,
                    headers: Vec::new(),
                },
                body,
            })
            .map_err(|error| UpstreamFailure {
                kind: UpstreamFailureKind::Transport,
                status: None,
                headers_received_at: None,
                upstream_request_id: None,
                client_response: None,
                retryable: error.is_retryable(),
                stable_error_code: "upstream_transport".to_string(),
                sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
            })
    }

    /// Metadata-preserving POST that leaves non-success status handling to a
    /// native protocol adapter. The default preserves compatibility with test
    /// and custom transports that only implement the legacy success-only API.
    async fn post_json_passthrough_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> std::result::Result<UpstreamResponse<AdmittedResponseText>, UpstreamFailure> {
        self.post_json_response(url, headers, body)
            .await
            .map(|response| response.map_body(AdmittedResponseText::unadmitted))
    }

    /// Streaming counterpart of `post_json_passthrough_response`.
    async fn post_stream_passthrough_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> std::result::Result<UpstreamResponse<ByteStream>, UpstreamFailure> {
        self.post_stream_response(url, headers, body).await
    }
    /// 发送 POST 请求并返回响应体
    async fn post_json(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> Result<String>;

    /// 发送 POST 请求并返回字节流（用于 SSE）
    async fn post_stream(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> Result<ByteStream>;

    /// 发送原始 POST 请求（自定义 Content-Type），用于 multipart/form-data 等场景
    ///
    /// 默认实现返回错误。需要处理二进制 body 的实现方（如 multipart/form-data）
    /// 必须显式覆盖此方法。不提供隐式回退到 `post_json`，因为：
    /// 1. multipart body 的二进制数据无法安全地通过 UTF-8 转换
    /// 2. Content-Type 语义不同（multipart/form-data vs application/json）
    /// 3. 静默回退会导致难以排查的运行时数据损坏
    async fn post_raw(
        &self,
        _url: &str,
        _headers: Vec<(String, String)>,
        _body: Vec<u8>,
    ) -> Result<String> {
        Err(keycompute_types::KeyComputeError::ProviderError(
            "post_raw is not supported by this transport implementation".into(),
        ))
    }

    /// 获取请求超时
    fn request_timeout(&self) -> Duration;

    /// 获取流式请求超时
    fn stream_timeout(&self) -> Duration;

    /// 发送 GET 请求并返回二进制响应体与 Content-Type
    ///
    /// 默认实现返回错误。需要处理二进制 GET 请求的实现方应覆盖此方法。
    /// 用于图片下载等场景，支持通过 Host header 实现 DNS 重绑定防护。
    /// 返回 `GetBinaryResponse` 包含 body 和 `content_type`，
    /// 便于调用方校验响应 MIME 类型（如图片下载后验证 `image/*`）。
    ///
    /// # 安全要求
    ///
    /// 实现方必须禁止 HTTP 重定向（`redirect::Policy::none()`），
    /// 防止 SSRF 攻击者通过 30x 重定向将请求引流至内网地址，
    /// 绕过调用方的 DNS 重绑定防护。
    async fn get_binary(
        &self,
        _url: &str,
        _headers: Vec<(String, String)>,
    ) -> Result<GetBinaryResponse> {
        Err(keycompute_types::KeyComputeError::ProviderError(
            "get_binary is not supported by this transport implementation".into(),
        ))
    }

    /// Metadata-preserving GET variant used by account probes. Legacy test or
    /// custom transports inherit a wrapper around `get_binary`; production
    /// transports override it to retain HTTP status and request correlation.
    async fn get_binary_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> std::result::Result<UpstreamResponse<GetBinaryResponse>, UpstreamFailure> {
        self.get_binary(url, headers)
            .await
            .map(|body| UpstreamResponse {
                meta: UpstreamResponseMeta::synthetic_success(),
                body,
            })
            .map_err(|error| UpstreamFailure {
                kind: UpstreamFailureKind::Transport,
                status: None,
                headers_received_at: None,
                upstream_request_id: None,
                client_response: None,
                retryable: error.is_retryable(),
                stable_error_code: "upstream_transport".to_string(),
                sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
            })
    }
}

/// GET 二进制响应
#[derive(Debug, Clone)]
pub struct GetBinaryResponse {
    /// 响应体字节
    pub body: Vec<u8>,
    /// Content-Type（从响应头提取）
    pub content_type: Option<String>,
}

/// 字节流类型
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

/// 默认 HTTP 传输实现（使用 reqwest）
#[derive(Debug, Clone)]
pub struct DefaultHttpTransport {
    client: reqwest::Client,
    request_timeout: Duration,
    stream_timeout: Duration,
}

impl Default for DefaultHttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultHttpTransport {
    /// 创建新的默认传输
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Failed to build HTTP client"),
            request_timeout: Duration::from_secs(120),
            stream_timeout: Duration::from_secs(600),
        }
    }

    /// 创建带自定义超时的传输
    pub fn with_timeouts(request_timeout: Duration, stream_timeout: Duration) -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Failed to build HTTP client"),
            request_timeout,
            stream_timeout,
        }
    }

    /// 构建请求
    fn build_request(
        &self,
        method: reqwest::Method,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> reqwest::RequestBuilder {
        let mut builder = self.client.request(method, url);
        for (key, value) in headers {
            builder = builder.header(key, value);
        }
        builder.body(body)
    }
}

#[async_trait]
impl HttpTransport for DefaultHttpTransport {
    async fn post_json_passthrough_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> std::result::Result<UpstreamResponse<AdmittedResponseText>, UpstreamFailure> {
        let response = self
            .build_request(reqwest::Method::POST, url, headers, body)
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|error| UpstreamFailure::transport(&error, false))?;
        let meta = response_meta(&response);
        let body = collect_bounded_response_text(
            response,
            &meta,
            json_passthrough_body_limit(meta.status),
        )
        .await?;
        Ok(UpstreamResponse { meta, body })
    }

    async fn post_stream_passthrough_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> std::result::Result<UpstreamResponse<ByteStream>, UpstreamFailure> {
        let response = self
            .build_request(reqwest::Method::POST, url, headers, body)
            .timeout(self.stream_timeout)
            .send()
            .await
            .map_err(|error| UpstreamFailure::transport(&error, false))?;
        let meta = response_meta(&response);
        let stream_status = meta.status;
        let stream = response.bytes_stream().map(move |result| {
            result.map_err(|error| keycompute_types::KeyComputeError::UpstreamFailure {
                status: Some(stream_status),
                stable_code: "upstream_stream_read".to_string(),
                retryable: false,
                summary: keycompute_types::sanitize_error_summary(&error.to_string()),
            })
        });
        Ok(UpstreamResponse {
            meta,
            body: Box::pin(stream),
        })
    }

    async fn post_json_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> std::result::Result<UpstreamResponse<String>, UpstreamFailure> {
        let response = self
            .build_request(reqwest::Method::POST, url, headers, body)
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|error| UpstreamFailure::transport(&error, false))?;
        let meta = response_meta(&response);
        if !response.status().is_success() {
            return Err(http_failure(response, meta).await);
        }
        let body = response.text().await.map_err(|error| UpstreamFailure {
            kind: UpstreamFailureKind::BodyRead,
            status: Some(meta.status),
            headers_received_at: Some(meta.headers_received_at),
            upstream_request_id: meta.upstream_request_id.clone(),
            client_response: None,
            // Headers from a successful paid POST make the outcome ambiguous:
            // the provider may have completed and charged the inference.
            retryable: false,
            stable_error_code: "upstream_body_read".to_string(),
            sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
        })?;
        Ok(UpstreamResponse { meta, body })
    }

    async fn post_stream_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> std::result::Result<UpstreamResponse<ByteStream>, UpstreamFailure> {
        let response = self
            .build_request(reqwest::Method::POST, url, headers, body)
            .timeout(self.stream_timeout)
            .send()
            .await
            .map_err(|error| UpstreamFailure::transport(&error, false))?;
        let meta = response_meta(&response);
        if !response.status().is_success() {
            return Err(http_failure(response, meta).await);
        }
        let stream_status = meta.status;
        let stream = response.bytes_stream().map(move |result| {
            result.map_err(|error| keycompute_types::KeyComputeError::UpstreamFailure {
                status: Some(stream_status),
                stable_code: "upstream_stream_read".to_string(),
                // Do not repeat a paid POST after the provider accepted it.
                retryable: false,
                summary: keycompute_types::sanitize_error_summary(&error.to_string()),
            })
        });
        Ok(UpstreamResponse {
            meta,
            body: Box::pin(stream),
        })
    }

    async fn post_json(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> Result<String> {
        self.post_json_response(url, headers, body)
            .await
            .map(|response| response.body)
            .map_err(UpstreamFailure::into_keycompute_error)
    }

    async fn post_stream(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: String,
    ) -> Result<ByteStream> {
        self.post_stream_response(url, headers, body)
            .await
            .map(|response| response.body)
            .map_err(UpstreamFailure::into_keycompute_error)
    }

    async fn post_raw(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<String> {
        let mut builder = self.client.post(url);
        for (key, value) in headers {
            builder = builder.header(key, value);
        }

        let response = builder
            .body(body)
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|error| UpstreamFailure::transport(&error, false).into_keycompute_error())?;

        let meta = response_meta(&response);
        if !response.status().is_success() {
            return Err(http_failure(response, meta).await.into_keycompute_error());
        }

        response
            .text()
            .await
            .map_err(|error| UpstreamFailure {
                kind: UpstreamFailureKind::BodyRead,
                status: Some(meta.status),
                headers_received_at: Some(meta.headers_received_at),
                upstream_request_id: meta.upstream_request_id,
                client_response: None,
                retryable: false,
                stable_error_code: "upstream_body_read".to_string(),
                sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
            })
            .map_err(UpstreamFailure::into_keycompute_error)
    }

    fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    fn stream_timeout(&self) -> Duration {
        self.stream_timeout
    }

    async fn get_binary(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<GetBinaryResponse> {
        self.get_binary_response(url, headers)
            .await
            .map(|response| response.body)
            .map_err(UpstreamFailure::into_keycompute_error)
    }

    async fn get_binary_response(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> std::result::Result<UpstreamResponse<GetBinaryResponse>, UpstreamFailure> {
        let mut builder = self.client.get(url);
        for (key, value) in headers {
            builder = builder.header(key, value);
        }

        let response = builder
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|error| UpstreamFailure::transport(&error, true))?;

        let meta = response_meta(&response);
        if !response.status().is_success() {
            return Err(http_failure(response, meta).await);
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let body = response.bytes().await.map_err(|error| UpstreamFailure {
            kind: UpstreamFailureKind::BodyRead,
            status: Some(meta.status),
            headers_received_at: Some(meta.headers_received_at),
            upstream_request_id: meta.upstream_request_id.clone(),
            client_response: None,
            retryable: true,
            stable_error_code: "upstream_body_read".to_string(),
            sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
        })?;
        Ok(UpstreamResponse {
            meta,
            body: GetBinaryResponse {
                body: body.to_vec(),
                content_type,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_transport_new() {
        let transport = DefaultHttpTransport::new();
        assert_eq!(transport.request_timeout(), Duration::from_secs(120));
        assert_eq!(transport.stream_timeout(), Duration::from_secs(600));
    }

    #[test]
    fn test_default_transport_with_timeouts() {
        let transport =
            DefaultHttpTransport::with_timeouts(Duration::from_secs(60), Duration::from_secs(300));
        assert_eq!(transport.request_timeout(), Duration::from_secs(60));
        assert_eq!(transport.stream_timeout(), Duration::from_secs(300));
    }

    #[test]
    fn json_passthrough_errors_use_the_smaller_body_limit() {
        assert_eq!(
            json_passthrough_body_limit(200),
            MAX_JSON_PASSTHROUGH_BODY_BYTES
        );
        assert_eq!(
            json_passthrough_body_limit(299),
            MAX_JSON_PASSTHROUGH_BODY_BYTES
        );
        assert_eq!(
            json_passthrough_body_limit(400),
            MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES
        );
        assert_eq!(
            json_passthrough_body_limit(599),
            MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES
        );
    }

    #[test]
    fn json_working_set_estimate_counts_structure_only_outside_strings() {
        let string_heavy = br#"{"payload":"[[[[,,,,::::{{{{"}"#;
        let array_heavy = br#"{"payload":[[],[],[],[],[]]}"#;

        assert_eq!(
            estimated_json_parse_working_set_bytes(string_heavy),
            string_heavy.len() * 2 + 2 * 64
        );
        assert!(
            estimated_json_parse_working_set_bytes(array_heavy)
                > estimated_json_parse_working_set_bytes(string_heavy)
        );
    }

    #[test]
    fn http_failure_summary_never_preserves_raw_body_details() {
        let body = br#"{"error":"client_secret=secret access_token=token prompt=private"}"#;
        let summary = summarize_http_failure_body(401, body);

        assert_eq!(summary, "Upstream returned HTTP 401");
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("token"));
        assert!(!summary.contains("private"));
    }

    #[test]
    fn http_failure_summary_only_preserves_stream_options_compatibility_signal() {
        let body = br#"{"error":"unknown field stream_options","client_secret":"secret"}"#;

        assert_eq!(
            summarize_http_failure_body(400, body),
            "Upstream rejected stream_options"
        );
        assert_eq!(
            summarize_http_failure_body(500, body),
            "Upstream returned HTTP 500"
        );
    }

    #[tokio::test]
    async fn admitted_response_retains_its_slot_through_json_ownership_transfer() {
        let slots = Arc::new(Semaphore::new(1));
        let permit = LargeBodyPermit {
            _permit: Arc::new(
                Arc::clone(&slots)
                    .acquire_owned()
                    .await
                    .expect("test semaphore remains open"),
            ),
        };
        let admitted = AdmittedResponseText {
            text: "{}".to_string(),
            permit: Some(permit),
        };
        assert!(Arc::clone(&slots).try_acquire_owned().is_err());

        let (text, permit) = admitted.into_parts();
        drop(text);
        assert!(Arc::clone(&slots).try_acquire_owned().is_err());

        drop(permit);
        assert!(Arc::clone(&slots).try_acquire_owned().is_ok());
    }

    #[test]
    fn bounded_passthrough_body_accepts_boundary_and_rejects_overflow() {
        let meta = UpstreamResponseMeta::synthetic_success();
        let mut body = vec![b'a'; 3];

        append_bounded_body(&mut body, b"b", 4, &meta).unwrap();
        assert_eq!(body, b"aaab");

        let error = append_bounded_body(&mut body, b"c", 4, &meta).unwrap_err();
        assert_eq!(error.kind, UpstreamFailureKind::BodyRead);
        assert_eq!(error.stable_error_code, "upstream_body_too_large");
        assert!(!error.retryable);
        assert_eq!(body, b"aaab");
    }

    #[test]
    fn late_large_body_admission_load_sheds_without_retaining_a_waiter() {
        let meta = UpstreamResponseMeta::synthetic_success();
        let mut permit = None::<()>;

        let error = try_admit_growing_large_body(
            true,
            LARGE_JSON_BODY_ADMISSION_BYTES,
            1,
            &mut permit,
            || None,
            &meta,
        )
        .unwrap_err();

        assert_eq!(error.kind, UpstreamFailureKind::BodyRead);
        assert_eq!(error.stable_error_code, "upstream_body_capacity_exhausted");
        assert!(!error.retryable);
        assert!(permit.is_none());
    }

    #[test]
    fn late_large_body_admission_only_reserves_when_crossing_the_threshold() {
        let meta = UpstreamResponseMeta::synthetic_success();
        let mut boundary_permit = None::<()>;
        try_admit_growing_large_body(
            true,
            LARGE_JSON_BODY_ADMISSION_BYTES - 1,
            1,
            &mut boundary_permit,
            || panic!("the small-body allowance should include the boundary"),
            &meta,
        )
        .unwrap();
        assert!(boundary_permit.is_none());

        let mut crossing_permit = None;
        try_admit_growing_large_body(
            true,
            LARGE_JSON_BODY_ADMISSION_BYTES,
            1,
            &mut crossing_permit,
            || Some(()),
            &meta,
        )
        .unwrap();
        assert_eq!(crossing_permit, Some(()));
    }

    #[test]
    fn declared_or_unknown_large_body_admission_load_sheds_without_waiting() {
        let meta = UpstreamResponseMeta::synthetic_success();

        for content_length in [None, Some(LARGE_JSON_BODY_ADMISSION_BYTES as u64 + 1)] {
            let error = try_admit_declared_or_unknown_large_body(
                true,
                content_length,
                || None::<()>,
                &meta,
            )
            .unwrap_err();

            assert_eq!(error.kind, UpstreamFailureKind::BodyRead);
            assert_eq!(error.stable_error_code, "upstream_body_capacity_exhausted");
            assert!(!error.retryable);
        }
    }

    #[test]
    fn declared_small_body_does_not_consume_large_body_capacity() {
        let meta = UpstreamResponseMeta::synthetic_success();
        let permit: Option<()> = try_admit_declared_or_unknown_large_body(
            true,
            Some(LARGE_JSON_BODY_ADMISSION_BYTES as u64),
            || panic!("the declared small-body boundary must not reserve a slot"),
            &meta,
        )
        .unwrap();

        assert!(permit.is_none());
    }

    #[test]
    fn bounded_body_failures_follow_http_status_retryability() {
        for (status, retryable) in [(400, false), (408, true), (429, true), (503, true)] {
            let mut meta = UpstreamResponseMeta::synthetic_success();
            meta.status = status;

            assert_eq!(
                body_read_failure(&meta, "upstream_body_read", "read failed").retryable,
                retryable,
                "unexpected retry classification for HTTP {status}"
            );
        }
    }

    #[test]
    fn bounded_response_rejects_invalid_utf8_without_lossy_replacement() {
        let meta = UpstreamResponseMeta::synthetic_success();
        let error = finish_bounded_response_text(vec![b'{', 0xff, b'}'], None, &meta)
            .expect_err("invalid UTF-8 must not be rewritten into response text");

        assert_eq!(error.kind, UpstreamFailureKind::BodyRead);
        assert_eq!(error.stable_error_code, "upstream_body_invalid_utf8");
        assert_eq!(
            error.sanitized_summary,
            "Upstream response body is not valid UTF-8"
        );
        assert!(!error.retryable);
    }
}
