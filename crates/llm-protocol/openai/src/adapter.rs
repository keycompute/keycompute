//! OpenAI 协议适配器实现
//!
//! 实现 ProviderAdapter trait，提供 OpenAI Chat Completions 与 Responses 调用能力，
//! 适用于所有 OpenAI 兼容上游（OpenAI/DeepSeek/Ollama/vLLM/Gemini 兼容层等）。
//! 支持 Chat Completions（含 Vision 多模态）、Responses、图片生成和图片编辑。
//!
//! 使用统一 HTTP 传输层：
//! - 通过 HttpTransport 发送请求
//! - 支持连接池复用和代理出口
//!
//! # 重要说明
//! - `endpoint` 为 Base URL（如 `https://api.openai.com/v1`），协议层负责拼接路径
//! - `endpoint` 和 `upstream_api_key` 由调用方通过 `UpstreamRequest` 传入
//! - 这些值通常从数据库 Account 表获取，而非配置文件
//! - 管理员可通过前端界面动态配置，无需重启系统

use async_trait::async_trait;
use futures::StreamExt;
use keycompute_types::{KeyComputeError, Result};
use llm_protocol_provider::{
    ByteStream, HttpTransport, LARGE_JSON_BODY_ADMISSION_BYTES,
    LARGE_JSON_WORKING_SET_ADMISSION_BYTES, MAX_JSON_PASSTHROUGH_BODY_BYTES,
    MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES, MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES,
    NativeResponsesRequest, NativeStreamEvent, ProviderAdapter, StreamBox, StreamEvent,
    UpstreamFailure, UpstreamFailureKind, UpstreamRequest, UpstreamResponse, UpstreamResponseMeta,
    body_read_failure, estimated_json_parse_working_set_bytes, http_status_is_retryable,
    try_acquire_large_body_permit,
};
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json;

use crate::protocol::{
    ImageEditRequest, ImageGenerationRequest, ImageGenerationResponse, ImageVariationRequest,
    OpenAIMessage, OpenAIRequest, OpenAIResponse, ResponsesRequest, ResponsesResponse,
    StreamOptions, convert_message_content,
};
use crate::responses_stream::{parse_responses_stream, response_usage, valid_response_status};
use crate::stream::{parse_native_openai_chat_stream, parse_openai_stream};

/// OpenAI Chat Completions 默认端点
pub const OPENAI_CHAT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

/// OpenAI 图片生成默认端点
pub const OPENAI_IMAGE_GEN_ENDPOINT: &str = "https://api.openai.com/v1/images/generations";

/// OpenAI 图片编辑默认端点
pub const OPENAI_IMAGE_EDIT_ENDPOINT: &str = "https://api.openai.com/v1/images/edits";

/// OpenAI 图片变体默认端点
pub const OPENAI_IMAGE_VARIATION_ENDPOINT: &str = "https://api.openai.com/v1/images/variations";

/// OpenAI Responses API 默认端点（统一多模态接口）
pub const OPENAI_RESPONSES_ENDPOINT: &str = "https://api.openai.com/v1/responses";

struct RoutedNativeChatBody<'a> {
    source: &'a serde_json::Map<String, serde_json::Value>,
    model: &'a str,
    stream: bool,
    include_stream_usage: bool,
}

impl Serialize for RoutedNativeChatBody<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        for (name, value) in self.source {
            if matches!(name.as_str(), "model" | "stream" | "stream_options") {
                continue;
            }
            map.serialize_entry(name, value)?;
        }
        map.serialize_entry("model", self.model)?;
        map.serialize_entry("stream", &self.stream)?;
        if self.stream && self.include_stream_usage {
            let mut options = self
                .source
                .get("stream_options")
                .and_then(serde_json::Value::as_object)
                .cloned()
                .unwrap_or_default();
            options.insert("include_usage".to_string(), serde_json::Value::Bool(true));
            map.serialize_entry("stream_options", &options)?;
        } else if !self.stream
            && let Some(options) = self.source.get("stream_options")
        {
            // Preserve the native request exactly outside the executor's one
            // explicit streaming compatibility retry. The upstream remains
            // authoritative if it considers stream_options invalid here.
            map.serialize_entry("stream_options", options)?;
        }
        map.end()
    }
}

struct RoutedNativeResponsesBody<'a> {
    source: &'a serde_json::Map<String, serde_json::Value>,
    model: &'a str,
    stream: Option<bool>,
}

impl Serialize for RoutedNativeResponsesBody<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        for (name, value) in self.source {
            if name == "model" || name == "stream" {
                continue;
            }
            map.serialize_entry(name, value)?;
        }
        if !self.model.is_empty() {
            map.serialize_entry("model", self.model)?;
        }
        if let Some(stream) = self.stream {
            map.serialize_entry("stream", &stream)?;
        }
        map.end()
    }
}

/// OpenAI Provider 适配器
#[derive(Debug, Clone)]
pub struct OpenAIProvider;

impl Default for OpenAIProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAIProvider {
    /// 创建新的 OpenAI Provider
    pub fn new() -> Self {
        Self
    }

    /// 拼接 Chat Completions URL
    ///
    /// `endpoint` 只存 Base URL（如 `https://api.openai.com/v1`），
    /// 路径由协议层统一拼接，不做任何“已含路径”兼容检测。
    fn chat_url(endpoint: &str) -> String {
        format!("{}/chat/completions", endpoint.trim_end_matches('/'))
    }

    /// 拼接 Responses API URL。
    fn responses_url(endpoint: &str, path: &str) -> String {
        format!("{}{}", endpoint.trim_end_matches('/'), path)
    }

    /// 判断是否为“可能由 stream_options 字段引发”的客户端错误
    ///
    /// 用于 stream_options 降级重试：部分 OpenAI 兼容上游（旧版 vLLM、
    /// 某些中转代理）不识别 `stream_options` 字段会返回 400/422。
    /// 有意排除 401/403/404/429 等与请求体无关的错误，
    /// 避免对无效 Key / 限流场景重发注定失败的请求。
    fn is_structured_client_error(err: &UpstreamFailure) -> bool {
        matches!(err.status, Some(400 | 422))
            && err
                .sanitized_summary
                .to_ascii_lowercase()
                .contains("stream_options")
    }

    fn protocol_failure(error: impl std::fmt::Display) -> UpstreamFailure {
        UpstreamFailure {
            kind: UpstreamFailureKind::Protocol,
            status: None,
            headers_received_at: None,
            upstream_request_id: None,
            client_response: None,
            retryable: false,
            stable_error_code: "upstream_protocol".to_string(),
            sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
        }
    }

    fn protocol_failure_with_meta(
        meta: &UpstreamResponseMeta,
        error: impl std::fmt::Display,
    ) -> UpstreamFailure {
        UpstreamFailure {
            kind: UpstreamFailureKind::Protocol,
            status: Some(meta.status),
            headers_received_at: Some(meta.headers_received_at),
            upstream_request_id: meta.upstream_request_id.clone(),
            client_response: None,
            retryable: false,
            stable_error_code: "upstream_protocol".to_string(),
            sanitized_summary: keycompute_types::sanitize_error_summary(&error.to_string()),
        }
    }

    fn validate_native_responses_json_working_set(
        meta: &UpstreamResponseMeta,
        body: &str,
        max_working_set_bytes: usize,
    ) -> std::result::Result<usize, UpstreamFailure> {
        let working_set_bytes = estimated_json_parse_working_set_bytes(body.as_bytes());
        if working_set_bytes > max_working_set_bytes {
            return Err(body_read_failure(
                meta,
                "upstream_json_too_complex",
                format!(
                    "Responses upstream JSON exceeds the {max_working_set_bytes}-byte working-set limit"
                ),
            ));
        }
        Ok(working_set_bytes)
    }

    /// 构建 OpenAI 请求体（支持 Vision 多模态）
    fn build_request_body(&self, request: &UpstreamRequest) -> OpenAIRequest {
        let messages: Vec<OpenAIMessage> = request
            .messages
            .iter()
            .map(|m| OpenAIMessage {
                role: m.role.clone(),
                content: Some(convert_message_content(m.content.clone())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            })
            .collect();

        OpenAIRequest {
            model: request.model.clone(),
            messages,
            stream: Some(request.stream),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            top_p: request.top_p,
            stop: None,
            stream_options: if request.stream && request.include_stream_usage {
                Some(StreamOptions {
                    include_usage: Some(true),
                })
            } else {
                None
            },
        }
    }

    fn serialize_native_chat_body(
        request: &UpstreamRequest,
    ) -> std::result::Result<String, UpstreamFailure> {
        let source = request
            .native_openai_chat_request
            .as_deref()
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                Self::protocol_failure("Native Chat Completions request must be a JSON object")
            })?;
        serde_json::to_string(&RoutedNativeChatBody {
            source,
            model: &request.model,
            stream: request.stream,
            include_stream_usage: request.include_stream_usage,
        })
        .map_err(Self::protocol_failure)
    }

    fn client_requested_chat_stream_usage(request: &UpstreamRequest) -> bool {
        request
            .native_openai_chat_request
            .as_deref()
            .and_then(|body| body.pointer("/stream_options/include_usage"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn chat_usage(value: &serde_json::Value) -> Option<(u32, u32)> {
        let usage = value.get("usage")?;
        let input_tokens = usage.get("prompt_tokens")?.as_u64()?;
        let output_tokens = usage.get("completion_tokens")?.as_u64()?;
        Some((
            u32::try_from(input_tokens).unwrap_or(u32::MAX),
            u32::try_from(output_tokens).unwrap_or(u32::MAX),
        ))
    }

    fn validate_native_chat_success(
        value: &serde_json::Value,
    ) -> std::result::Result<(), &'static str> {
        let Some(object) = value.as_object() else {
            return Err("Chat Completions response must be a JSON object");
        };
        if object.get("object").and_then(serde_json::Value::as_str) != Some("chat.completion") {
            return Err("Chat Completions response has an invalid object type");
        }
        if !object.get("id").is_some_and(|id| {
            id.as_str().is_some_and(|id| {
                !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
            })
        }) {
            return Err("Chat Completions response has an invalid id");
        }
        if !object
            .get("created")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|created| created >= 0)
        {
            return Err("Chat Completions response has an invalid created timestamp");
        }
        if !object
            .get("model")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|model| !model.is_empty())
        {
            return Err("Chat Completions response has an invalid model");
        }
        let Some(choices) = object.get("choices").and_then(serde_json::Value::as_array) else {
            return Err("Chat Completions response choices must be an array");
        };
        for choice in choices {
            let Some(choice) = choice.as_object() else {
                return Err("Chat Completions response choices must contain objects");
            };
            if !choice
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .is_some()
            {
                return Err("Chat Completions response choice has an invalid index");
            }
            let Some(message) = choice.get("message").and_then(serde_json::Value::as_object) else {
                return Err("Chat Completions response choice has an invalid message");
            };
            if message.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
                return Err("Chat Completions response message has an invalid role");
            }
        }
        Ok(())
    }

    fn native_chat_http_failure(meta: &UpstreamResponseMeta, body: &str) -> UpstreamFailure {
        let status = meta.status;
        let mentions_stream_options =
            matches!(status, 400 | 422) && body.to_ascii_lowercase().contains("stream_options");
        UpstreamFailure {
            kind: UpstreamFailureKind::HttpStatus,
            status: Some(status),
            headers_received_at: Some(meta.headers_received_at),
            upstream_request_id: meta.upstream_request_id.clone(),
            client_response: Some(Box::new(keycompute_types::ClientUpstreamResponse {
                status,
                headers: meta.headers.clone(),
                body: body.chars().take(8 * 1024).collect(),
            })),
            retryable: http_status_is_retryable(status),
            stable_error_code: format!("upstream_http_{status}"),
            sanitized_summary: if mentions_stream_options {
                "Upstream rejected stream_options".to_string()
            } else {
                format!("Upstream returned HTTP {status}")
            },
        }
    }

    async fn native_chat_with_meta(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse<StreamBox>, UpstreamFailure> {
        let body_json = Self::serialize_native_chat_body(&request)?;
        let mut headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let url = Self::chat_url(&request.endpoint);

        if request.stream {
            headers.push(("Accept".to_string(), "text/event-stream".to_string()));
            let response = match transport
                .post_stream_response(&url, headers, body_json)
                .await
            {
                Ok(response) => response,
                Err(mut error)
                    if request.include_stream_usage && Self::is_structured_client_error(&error) =>
                {
                    error.retryable = true;
                    error.stable_error_code = "upstream_stream_options_unsupported".to_string();
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
            let forward_usage = Self::client_requested_chat_stream_usage(&request);
            return Ok(
                response.map_body(|stream| parse_native_openai_chat_stream(stream, forward_usage))
            );
        }

        let response = transport
            .post_json_passthrough_response(&url, headers, body_json)
            .await?;
        if !(200..300).contains(&response.meta.status) {
            return Err(Self::native_chat_http_failure(
                &response.meta,
                &response.body,
            ));
        }
        let (body, mut admission) = response.body.into_parts();
        if body.len() > MAX_JSON_PASSTHROUGH_BODY_BYTES {
            return Err(body_read_failure(
                &response.meta,
                "upstream_body_too_large",
                format!(
                    "Chat Completions upstream JSON exceeds the {}-byte limit",
                    MAX_JSON_PASSTHROUGH_BODY_BYTES
                ),
            ));
        }
        let working_set_bytes = estimated_json_parse_working_set_bytes(body.as_bytes());
        if working_set_bytes > MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES {
            return Err(body_read_failure(
                &response.meta,
                "upstream_json_too_complex",
                format!(
                    "Chat Completions upstream JSON exceeds the {}-byte working-set limit",
                    MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES
                ),
            ));
        }
        if admission.is_none()
            && (body.len() > LARGE_JSON_BODY_ADMISSION_BYTES
                || working_set_bytes > LARGE_JSON_WORKING_SET_ADMISSION_BYTES)
        {
            admission = Some(try_acquire_large_body_permit().ok_or_else(|| {
                body_read_failure(
                    &response.meta,
                    "upstream_json_capacity_exhausted",
                    "Chat Completions upstream JSON working-set capacity is exhausted",
                )
            })?);
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|error| Self::protocol_failure_with_meta(&response.meta, error))?;
        drop(body);
        Self::validate_native_chat_success(&value)
            .map_err(|error| Self::protocol_failure_with_meta(&response.meta, error))?;
        let usage = Self::chat_usage(&value);
        let mut events = vec![Ok(StreamEvent::native(NativeStreamEvent::OpenAiChatJson {
            body: value,
            admission,
        }))];
        if let Some((input_tokens, output_tokens)) = usage {
            events.push(Ok(StreamEvent::usage(input_tokens, output_tokens)));
        }
        events.push(Ok(StreamEvent::done()));
        Ok(UpstreamResponse {
            meta: response.meta,
            body: Box::pin(futures::stream::iter(events)),
        })
    }

    /// Build the final native Responses payload. The public handler retains
    /// all official and future fields; only routing-owned fields are forced at
    /// the last possible moment.
    fn serialize_native_responses_body(
        request: &UpstreamRequest,
        native_request: &NativeResponsesRequest,
    ) -> std::result::Result<String, UpstreamFailure> {
        let object = native_request.body.as_object().ok_or_else(|| {
            Self::protocol_failure("Native Responses request must be a JSON object")
        })?;
        serde_json::to_string(&RoutedNativeResponsesBody {
            source: object,
            model: &request.model,
            stream: (native_request.path == "/responses").then_some(request.stream),
        })
        .map_err(Self::protocol_failure)
    }

    fn native_responses_headers(
        request: &UpstreamRequest,
        native_request: &NativeResponsesRequest,
    ) -> Vec<(String, String)> {
        let mut headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        for name in ["idempotency-key", "openai-beta", "x-client-request-id"] {
            if let Some(value) = native_request.headers.get(name) {
                headers.push((name.to_string(), value.clone()));
            }
        }
        headers
    }

    fn validate_native_responses_success(
        path: &str,
        value: &serde_json::Value,
    ) -> std::result::Result<(), &'static str> {
        let Some(object) = value.as_object() else {
            return Err("Responses response must be a JSON object");
        };
        let expected_object = match path {
            "/responses" => "response",
            "/responses/compact" => "response.compaction",
            _ => return Err("Unsupported native Responses resource path"),
        };
        if object.get("object").and_then(serde_json::Value::as_str) != Some(expected_object) {
            return Err("Responses response has an invalid object type");
        }
        if object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|id| !crate::responses_stream::valid_openai_resource_id(id))
        {
            return Err("Responses response is missing a valid id");
        }
        if !object
            .get("output")
            .is_some_and(serde_json::Value::is_array)
        {
            return Err("Responses response output must be an array");
        }
        if path == "/responses"
            && object
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|status| !valid_response_status(status))
        {
            return Err("Responses response is missing a valid status");
        }
        Ok(())
    }

    fn native_responses_error_stream(
        meta: UpstreamResponseMeta,
        body: String,
    ) -> UpstreamResponse<StreamBox> {
        let headers = meta
            .headers
            .iter()
            .filter(|(name, _)| {
                let name = name.to_ascii_lowercase();
                name == "content-type"
                    || name == "x-request-id"
                    || name == "request-id"
                    || name == "openai-version"
                    || name == "openai-processing-ms"
                    || name == "retry-after"
                    || name.starts_with("x-ratelimit-")
            })
            .cloned()
            .collect::<Vec<_>>();
        let event = NativeStreamEvent::OpenAiResponsesHttpError {
            status: meta.status,
            headers,
            body,
        };
        UpstreamResponse {
            meta,
            body: Box::pin(futures::stream::once(async move {
                Ok(StreamEvent::native(event))
            })),
        }
    }

    async fn collect_native_responses_error_body(
        mut stream: ByteStream,
        meta: &UpstreamResponseMeta,
        max_bytes: usize,
    ) -> std::result::Result<String, UpstreamFailure> {
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                body_read_failure(meta, "upstream_body_read", error.to_string())
            })?;
            if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
                return Err(body_read_failure(
                    meta,
                    "upstream_body_too_large",
                    format!("Responses upstream error body exceeds the {max_bytes}-byte limit"),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn native_responses_with_meta(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
        native_request: NativeResponsesRequest,
    ) -> std::result::Result<UpstreamResponse<StreamBox>, UpstreamFailure> {
        let path = native_request.path.as_str();
        if !matches!(path, "/responses" | "/responses/compact") {
            return Err(Self::protocol_failure(
                "Unsupported native Responses resource path",
            ));
        }
        if request.stream && path != "/responses" {
            return Err(Self::protocol_failure(
                "Streaming is only supported for /responses",
            ));
        }
        // Serialize directly from the shared request object while overlaying
        // only routing-owned fields. This avoids deep-cloning requests that
        // may contain tens of MiB of inline skill data.
        let body_json = Self::serialize_native_responses_body(&request, &native_request)?;
        let url = Self::responses_url(&request.endpoint, path);
        let mut headers = Self::native_responses_headers(&request, &native_request);

        if request.stream {
            headers.push(("Accept".to_string(), "text/event-stream".to_string()));
            let response = transport
                .post_stream_passthrough_response(&url, headers, body_json)
                .await?;
            if !(200..300).contains(&response.meta.status) {
                let meta = response.meta;
                let body = Self::collect_native_responses_error_body(
                    response.body,
                    &meta,
                    MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES,
                )
                .await?;
                return Ok(Self::native_responses_error_stream(meta, body));
            }
            return Ok(response.map_body(parse_responses_stream));
        }

        let response = transport
            .post_json_passthrough_response(&url, headers, body_json)
            .await?;
        if !(200..300).contains(&response.meta.status) {
            if response.body.len() > MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES {
                return Err(body_read_failure(
                    &response.meta,
                    "upstream_body_too_large",
                    format!(
                        "Responses upstream error body exceeds the {}-byte limit",
                        MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES
                    ),
                ));
            }
            return Ok(Self::native_responses_error_stream(
                response.meta,
                response.body.into_string(),
            ));
        }
        let (body, mut admission) = response.body.into_parts();
        let working_set_bytes = Self::validate_native_responses_json_working_set(
            &response.meta,
            &body,
            MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES,
        )?;
        if admission.is_none() && working_set_bytes > LARGE_JSON_WORKING_SET_ADMISSION_BYTES {
            admission = Some(try_acquire_large_body_permit().ok_or_else(|| {
                body_read_failure(
                    &response.meta,
                    "upstream_json_capacity_exhausted",
                    "Responses upstream JSON working-set capacity is exhausted",
                )
            })?);
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|error| Self::protocol_failure_with_meta(&response.meta, error))?;
        drop(body);
        Self::validate_native_responses_success(path, &value)
            .map_err(|error| Self::protocol_failure_with_meta(&response.meta, error))?;
        let usage = response_usage(&value)
            .map_err(|error| Self::protocol_failure_with_meta(&response.meta, error))?;
        let mut events: Vec<Result<StreamEvent>> = vec![Ok(StreamEvent::native(
            NativeStreamEvent::OpenAiResponsesJson {
                body: value,
                admission,
            },
        ))];
        if let Some((input_tokens, output_tokens)) = usage {
            events.push(Ok(StreamEvent::Usage {
                input_tokens,
                output_tokens,
            }));
        }
        events.push(Ok(StreamEvent::Done));
        Ok(UpstreamResponse {
            meta: response.meta,
            body: Box::pin(futures::stream::iter(events)),
        })
    }

    /// 执行非流式请求
    async fn chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<(String, Option<(u32, u32)>, Option<String>)> {
        self.chat_internal_with_meta(transport, request)
            .await
            .map(|response| response.body)
            .map_err(UpstreamFailure::into_keycompute_error)
    }

    async fn chat_internal_with_meta(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> std::result::Result<
        UpstreamResponse<(String, Option<(u32, u32)>, Option<String>)>,
        UpstreamFailure,
    > {
        let body = self.build_request_body(&request);
        let body_json = serde_json::to_string(&body).map_err(Self::protocol_failure)?;

        let headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];

        let response = transport
            .post_json_response(&Self::chat_url(&request.endpoint), headers, body_json)
            .await?;

        let openai_response: OpenAIResponse = serde_json::from_str(&response.body)
            .map_err(|error| Self::protocol_failure_with_meta(&response.meta, error))?;

        // 使用 OpenAIResponse 的方法一次性提取所有字段
        let content = openai_response.extract_text();
        let finish_reason = openai_response.extract_finish_reason();

        // 提取 usage 信息（非流式响应通常包含完整的 usage 数据）
        let usage = openai_response
            .usage
            .map(|u| (u.prompt_tokens as u32, u.completion_tokens as u32));

        Ok(UpstreamResponse {
            meta: response.meta,
            body: (content, usage, finish_reason),
        })
    }

    /// 执行流式请求
    async fn stream_chat_internal_with_meta(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse<StreamBox>, UpstreamFailure> {
        let body = self.build_request_body(&request);
        let body_json = serde_json::to_string(&body).map_err(Self::protocol_failure)?;

        let headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
        ];

        let url = Self::chat_url(&request.endpoint);
        let response = match transport
            .post_stream_response(&url, headers, body_json)
            .await
        {
            Ok(response) => response,
            // 将兼容性重试边界交给 executor。这样第一次 400/422 的状态、
            // Request ID、耗时和随后不带 stream_options 的请求各自拥有 attempt。
            Err(mut error)
                if body.stream_options.is_some() && Self::is_structured_client_error(&error) =>
            {
                error.retryable = true;
                error.stable_error_code = "upstream_stream_options_unsupported".to_string();
                return Err(error);
            }
            Err(e) => return Err(e),
        };

        // 转换字节流为 SSE 事件流
        Ok(response.map_body(parse_openai_stream))
    }

    // ========================================================================
    // 图片生成
    // ========================================================================

    /// 构建 JSON API 通用请求头（Authorization + Content-Type: application/json）
    ///
    /// 用于 generate_image、create_response 等使用 JSON body 的非流式端点。
    /// multipart 场景（edit_image / create_image_variation）使用 `build_auth_header`
    /// 单独构建 Authorization 头，因为 Content-Type 需要动态设置为 multipart 边界值。
    fn build_json_api_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }

    /// 执行图片生成
    pub async fn generate_image(
        &self,
        transport: &dyn HttpTransport,
        endpoint: &str,
        api_key: &str,
        request: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse> {
        let body_json = serde_json::to_string(&request).map_err(|e| {
            KeyComputeError::ProviderError(format!(
                "Failed to serialize image generation request: {}",
                e
            ))
        })?;

        let headers = self.build_json_api_headers(api_key);

        let response_text = transport.post_json(endpoint, headers, body_json).await?;

        let response: ImageGenerationResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                KeyComputeError::ProviderError(format!(
                    "Failed to parse image generation response: {}",
                    e
                ))
            })?;

        Ok(response)
    }

    /// 执行图片生成（使用默认端点）
    pub async fn generate_image_default(
        &self,
        transport: &dyn HttpTransport,
        api_key: &str,
        request: ImageGenerationRequest,
    ) -> Result<ImageGenerationResponse> {
        self.generate_image(transport, OPENAI_IMAGE_GEN_ENDPOINT, api_key, request)
            .await
    }

    // ========================================================================
    // 图片编辑
    // ========================================================================

    /// 构建 multipart/form-data 请求体
    ///
    /// 手动构建 multipart body，避免在 provider adapter 层引入 reqwest 依赖。
    ///
    /// # 安全
    /// - 所有字段值、文件名、content_type 中的控制字符（如 \r\n）会被过滤，防止 CRLF 注入攻击
    /// - 引号和反斜杠也会被过滤，防止引用逃逸
    fn build_multipart_body(
        text_fields: &[(&str, &str)],
        file_fields: &[(&str, &str, &str, &[u8])], // (name, filename, content_type, data)
    ) -> (Vec<u8>, String) {
        let boundary = format!("----KeyComputeBoundary{}", uuid::Uuid::new_v4().as_simple());
        let mut body = Vec::new();

        // 清理 header 值中的危险字符：ASCII 控制字符（\r\n）和引号
        // 仅用于 multipart header（name/filename/content_type），防止 CRLF 注入和引用逃逸。
        // 不过滤反斜杠 `\`，因为：
        // 1. CRLF 注入防护核心是过滤 \r / \n（已由 is_ascii_control 覆盖）
        // 2. filename 可能包含合法反斜杠（如 Unix 路径），不应静默修改
        // 3. Content-Disposition quoted-string 中仅需过滤 `"` 即可防止逃逸
        // 注意：文本字段的 body 值（如 prompt）不应调用此函数，因为：
        // 1. boundary 是随机 UUID，绝无碰撞可能
        // 2. prompt 中的引号和反斜杠是合法的用户输入
        fn sanitize_header_value(s: &str) -> String {
            s.chars()
                .filter(|c| !c.is_ascii_control() && *c != '"')
                .collect()
        }

        // 文本字段：name 做 header 安全过滤，但 value（如 prompt）不过滤
        for (name, value) in text_fields {
            let sanitized_name = sanitize_header_value(name);
            body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                    sanitized_name
                )
                .as_bytes(),
            );
            body.extend_from_slice(value.as_bytes());
            body.extend_from_slice(b"\r\n");
        }

        // 文件字段：name/filename/content_type 都需要 header 安全过滤
        for (name, filename, content_type, data) in file_fields {
            let sanitized_name = sanitize_header_value(name);
            let sanitized_filename = sanitize_header_value(filename);
            let sanitized_content_type = sanitize_header_value(content_type);

            body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                    sanitized_name, sanitized_filename
                )
                .as_bytes(),
            );
            body.extend_from_slice(
                format!("Content-Type: {}\r\n\r\n", sanitized_content_type).as_bytes(),
            );
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }

        // 结束边界
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

        let content_type = format!("multipart/form-data; boundary={}", boundary);
        (body, content_type)
    }

    /// 构建图片 API 通用文本字段列表
    ///
    /// 图片编辑和图片变体 API 共享相同的可选参数（model, n, size,
    /// response_format, user），此方法提取它们的通用构建逻辑。
    /// extra_fields 用于添加非共享字段（如 edit 的 prompt）。
    fn build_image_text_fields(
        model: &Option<String>,
        n: Option<u32>,
        size: &Option<String>,
        response_format: &Option<String>,
        user: &Option<String>,
        extra_fields: &[(&str, String)],
    ) -> Vec<(String, String)> {
        let mut text_fields: Vec<(String, String)> = extra_fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        if let Some(model) = model {
            text_fields.push(("model".to_string(), model.clone()));
        }
        if let Some(n) = n {
            text_fields.push(("n".to_string(), n.to_string()));
        }
        if let Some(size) = size {
            text_fields.push(("size".to_string(), size.clone()));
        }
        if let Some(fmt) = response_format {
            text_fields.push(("response_format".to_string(), fmt.clone()));
        }
        if let Some(user) = user {
            text_fields.push(("user".to_string(), user.clone()));
        }
        text_fields
    }

    /// 构建图片 API 请求头（multipart 场景需要动态 Content-Type，不在此设置）
    fn build_auth_header(&self, api_key: &str) -> (String, String) {
        ("Authorization".to_string(), format!("Bearer {}", api_key))
    }

    /// 执行 multipart/form-data 图片请求的通用逻辑
    ///
    /// `edit_image` 和 `create_image_variation` 共享的核心流程：
    /// 构建文本字段 → 构建文件字段 → 构建 multipart body → 发送请求 → 解析 JSON
    #[allow(clippy::too_many_arguments)]
    async fn execute_image_multipart_request(
        &self,
        transport: &dyn HttpTransport,
        endpoint: &str,
        api_key: &str,
        model: &Option<String>,
        n: Option<u32>,
        size: &Option<String>,
        response_format: &Option<String>,
        user: &Option<String>,
        extra_text_fields: &[(&str, String)],
        file_fields: &[(&str, &str, &str, &[u8])],
        error_label: &str,
    ) -> Result<ImageGenerationResponse> {
        let text_fields =
            Self::build_image_text_fields(model, n, size, response_format, user, extra_text_fields);
        let text_refs: Vec<(&str, &str)> = text_fields
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let (body, content_type) = Self::build_multipart_body(&text_refs, file_fields);

        let auth_header = self.build_auth_header(api_key);
        let headers = vec![auth_header, ("Content-Type".to_string(), content_type)];

        let response_text = transport.post_raw(endpoint, headers, body).await?;

        let response: ImageGenerationResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                KeyComputeError::ProviderError(format!(
                    "Failed to parse {} response: {}",
                    error_label, e
                ))
            })?;

        Ok(response)
    }

    /// 执行图片编辑（使用 multipart/form-data）
    ///
    /// OpenAI /v1/images/edits 端点的正确调用方式：
    /// - prompt: 文本字段
    /// - image: 文件字段（原始图片二进制）
    /// - mask: 文件字段（可选，遮罩图片二进制）
    /// - model/n/size/response_format/user: 文本字段
    pub async fn edit_image(
        &self,
        transport: &dyn HttpTransport,
        endpoint: &str,
        api_key: &str,
        request: ImageEditRequest,
    ) -> Result<ImageGenerationResponse> {
        let stored_prompt = request.prompt.clone();
        let extra_fields = [("prompt", stored_prompt)];

        let mut file_fields: Vec<(&str, &str, &str, &[u8])> = vec![(
            "image",
            request.image_filename.as_str(),
            request.image_content_type.as_str(),
            &request.image,
        )];

        // mask 字段需要存活到 file_fields 使用完毕
        let mask_fn;
        let mask_ct;
        if let Some(ref mask) = request.mask {
            mask_fn = request
                .mask_filename
                .as_deref()
                .unwrap_or("mask.png")
                .to_string();
            mask_ct = request
                .mask_content_type
                .as_deref()
                .unwrap_or("image/png")
                .to_string();
            file_fields.push(("mask", mask_fn.as_str(), mask_ct.as_str(), mask));
        }

        self.execute_image_multipart_request(
            transport,
            endpoint,
            api_key,
            &request.model,
            request.n,
            &request.size,
            &request.response_format,
            &request.user,
            &extra_fields,
            &file_fields,
            "image edit",
        )
        .await
    }

    /// 执行图片编辑（使用默认端点）
    pub async fn edit_image_default(
        &self,
        transport: &dyn HttpTransport,
        api_key: &str,
        request: ImageEditRequest,
    ) -> Result<ImageGenerationResponse> {
        self.edit_image(transport, OPENAI_IMAGE_EDIT_ENDPOINT, api_key, request)
            .await
    }

    // ========================================================================
    // 图片变体
    // ========================================================================

    /// 执行图片变体请求（使用 multipart/form-data）
    pub async fn create_image_variation(
        &self,
        transport: &dyn HttpTransport,
        endpoint: &str,
        api_key: &str,
        request: ImageVariationRequest,
    ) -> Result<ImageGenerationResponse> {
        let file_fields: Vec<(&str, &str, &str, &[u8])> = vec![(
            "image",
            request.image_filename.as_str(),
            request.image_content_type.as_str(),
            &request.image,
        )];

        self.execute_image_multipart_request(
            transport,
            endpoint,
            api_key,
            &request.model,
            request.n,
            &request.size,
            &request.response_format,
            &request.user,
            &[],
            &file_fields,
            "image variation",
        )
        .await
    }

    /// 执行图片变体（使用默认端点）
    pub async fn create_image_variation_default(
        &self,
        transport: &dyn HttpTransport,
        api_key: &str,
        request: ImageVariationRequest,
    ) -> Result<ImageGenerationResponse> {
        self.create_image_variation(transport, OPENAI_IMAGE_VARIATION_ENDPOINT, api_key, request)
            .await
    }

    // ========================================================================
    // Responses API（统一多模态接口）
    // ========================================================================

    /// 执行 Responses API 非流式请求
    ///
    /// Responses API 是 OpenAI 最新的统一接口，支持：
    /// - 文本 + 图片多模态输入
    /// - 工具调用（image_generation, web_search, file_search）
    /// - 状态保持
    pub async fn create_response(
        &self,
        transport: &dyn HttpTransport,
        endpoint: &str,
        api_key: &str,
        request: ResponsesRequest,
    ) -> Result<ResponsesResponse> {
        let body_json = serde_json::to_string(&request).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to serialize responses request: {}", e))
        })?;

        let headers = self.build_json_api_headers(api_key);

        let response_text = transport.post_json(endpoint, headers, body_json).await?;

        let response: ResponsesResponse = serde_json::from_str(&response_text).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to parse responses response: {}", e))
        })?;

        Ok(response)
    }

    /// 执行 Responses API 请求（使用默认端点）
    pub async fn create_response_default(
        &self,
        transport: &dyn HttpTransport,
        api_key: &str,
        request: ResponsesRequest,
    ) -> Result<ResponsesResponse> {
        self.create_response(transport, OPENAI_RESPONSES_ENDPOINT, api_key, request)
            .await
    }

    /// 执行 Responses API 流式请求
    pub async fn stream_response(
        &self,
        transport: &dyn HttpTransport,
        endpoint: &str,
        api_key: &str,
        request: ResponsesRequest,
    ) -> Result<StreamBox> {
        let mut stream_req = request;
        stream_req.stream = Some(true);

        let body_json = serde_json::to_string(&stream_req).map_err(|e| {
            KeyComputeError::ProviderError(format!(
                "Failed to serialize responses stream request: {}",
                e
            ))
        })?;

        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
        ];

        let byte_stream: ByteStream = transport.post_stream(endpoint, headers, body_json).await?;

        Ok(parse_openai_stream(byte_stream))
    }

    /// 执行 Responses API 流式请求（使用默认端点）
    pub async fn stream_response_default(
        &self,
        transport: &dyn HttpTransport,
        api_key: &str,
        request: ResponsesRequest,
    ) -> Result<StreamBox> {
        self.stream_response(transport, OPENAI_RESPONSES_ENDPOINT, api_key, request)
            .await
    }
}

#[async_trait]
impl ProviderAdapter for OpenAIProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        // 协议层不维护模型白名单，模型由渠道账号的 models_supported 声明
        Vec::new()
    }

    /// 协议层接受任意模型，路由层已按账号 models_supported 过滤
    fn supports_model(&self, _model: &str) -> bool {
        true
    }

    /// OpenAI 原生支持图片生成（DALL-E）
    fn supports_image_generation(&self) -> bool {
        true
    }

    /// OpenAI 原生支持图片编辑
    fn supports_image_editing(&self) -> bool {
        true
    }

    async fn stream_chat(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<StreamBox> {
        if request.native_openai_chat_request.is_some() {
            return self
                .native_chat_with_meta(transport, request)
                .await
                .map(|response| response.body)
                .map_err(UpstreamFailure::into_keycompute_error);
        }
        if request.stream {
            self.stream_chat_internal_with_meta(transport, request)
                .await
                .map(|response| response.body)
                .map_err(|error| KeyComputeError::ProviderError(error.to_string()))
        } else {
            // 非流式请求，包装为单事件流
            let (content, usage, finish_reason) = self.chat_internal(transport, request).await?;

            let event = StreamEvent::Delta {
                content,
                // 非流式响应有finish_reason，设为Some
                finish_reason: Some(finish_reason.unwrap_or_else(|| "stop".to_string())),
            };

            let mut events: Vec<Result<StreamEvent>> = vec![Ok(event)];

            // 如果有 usage 信息，添加 Usage 事件
            if let Some((input_tokens, output_tokens)) = usage {
                events.push(Ok(StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                }));
            }

            events.push(Ok(StreamEvent::Done));

            let stream = futures::stream::iter(events);
            Ok(Box::pin(stream))
        }
    }

    async fn stream_chat_with_meta(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> std::result::Result<UpstreamResponse<StreamBox>, UpstreamFailure> {
        if request.native_openai_chat_request.is_some() {
            return self.native_chat_with_meta(transport, request).await;
        }
        if request.stream {
            self.stream_chat_internal_with_meta(transport, request)
                .await
        } else {
            let response = self.chat_internal_with_meta(transport, request).await?;
            let (content, usage, finish_reason) = response.body;
            let mut events: Vec<Result<StreamEvent>> = vec![Ok(StreamEvent::Delta {
                content,
                finish_reason: Some(finish_reason.unwrap_or_else(|| "stop".to_string())),
            })];
            if let Some((input_tokens, output_tokens)) = usage {
                events.push(Ok(StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                }));
            }
            events.push(Ok(StreamEvent::Done));
            Ok(UpstreamResponse {
                meta: response.meta,
                body: Box::pin(futures::stream::iter(events)),
            })
        }
    }

    async fn stream_responses_with_meta(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
        native_request: NativeResponsesRequest,
    ) -> std::result::Result<UpstreamResponse<StreamBox>, UpstreamFailure> {
        self.native_responses_with_meta(transport, request, native_request)
            .await
    }

    async fn chat(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<String> {
        let (content, _usage, _finish_reason) = self.chat_internal(transport, request).await?;
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_protocol_provider::ByteStream;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Mock 传输层：首次 post_stream 返回指定 HTTP 错误，后续调用成功，
    /// 并记录每次请求的 body 用于断言 stream_options 降级行为
    #[derive(Debug)]
    struct DegradingTransport {
        /// 首次请求返回的错误消息
        first_error: String,
        /// 记录收到的请求 body
        bodies: Mutex<Vec<String>>,
    }

    impl DegradingTransport {
        fn new(first_error: impl Into<String>) -> Self {
            Self {
                first_error: first_error.into(),
                bodies: Mutex::new(Vec::new()),
            }
        }
    }

    #[derive(Debug)]
    struct MetadataTransport {
        fail_stream: bool,
    }

    #[derive(Debug)]
    struct NativeResponsesTransport {
        url: Mutex<Option<String>>,
        headers: Mutex<Vec<(String, String)>>,
        body: Mutex<Option<String>>,
        response: String,
        status: u16,
    }

    #[async_trait]
    impl HttpTransport for NativeResponsesTransport {
        async fn post_json_response(
            &self,
            url: &str,
            headers: Vec<(String, String)>,
            body: String,
        ) -> std::result::Result<UpstreamResponse<String>, UpstreamFailure> {
            *self.url.lock().unwrap() = Some(url.to_string());
            *self.headers.lock().unwrap() = headers;
            *self.body.lock().unwrap() = Some(body);
            let mut meta = UpstreamResponseMeta::synthetic_success();
            meta.status = self.status;
            meta.headers = vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("set-cookie".to_string(), "secret=hidden".to_string()),
            ];
            Ok(UpstreamResponse {
                meta,
                body: self.response.clone(),
            })
        }

        async fn post_stream_response(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> std::result::Result<UpstreamResponse<ByteStream>, UpstreamFailure> {
            unreachable!("non-stream Responses test")
        }

        async fn post_json(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> Result<String> {
            unreachable!("structured method must be used")
        }

        async fn post_stream(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> Result<ByteStream> {
            unreachable!("structured method must be used")
        }

        fn request_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn stream_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }
    }

    #[async_trait]
    impl HttpTransport for MetadataTransport {
        async fn post_json_response(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> std::result::Result<UpstreamResponse<String>, UpstreamFailure> {
            let mut meta = UpstreamResponseMeta::synthetic_success();
            meta.upstream_request_id = Some("upstream-json-id".to_string());
            Ok(UpstreamResponse {
                meta,
                body: "not-json".to_string(),
            })
        }

        async fn post_stream_response(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> std::result::Result<UpstreamResponse<ByteStream>, UpstreamFailure> {
            if self.fail_stream {
                let received_at = UpstreamResponseMeta::synthetic_success().headers_received_at;
                Err(UpstreamFailure {
                    kind: UpstreamFailureKind::HttpStatus,
                    status: Some(429),
                    headers_received_at: Some(received_at),
                    upstream_request_id: Some("upstream-rate-id".to_string()),
                    client_response: None,
                    retryable: true,
                    stable_error_code: "upstream_http_429".to_string(),
                    sanitized_summary: "rate limited".to_string(),
                })
            } else {
                unreachable!("stream response is not used by the non-stream test")
            }
        }

        async fn post_json(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> Result<String> {
            unreachable!("structured method must be used")
        }

        async fn post_stream(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> Result<ByteStream> {
            unreachable!("structured method must be used")
        }

        fn request_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn stream_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }
    }

    #[async_trait]
    impl HttpTransport for DegradingTransport {
        async fn post_stream_response(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            body: String,
        ) -> std::result::Result<UpstreamResponse<ByteStream>, UpstreamFailure> {
            let mut bodies = self.bodies.lock().unwrap();
            let has_stream_options = body.contains("stream_options");
            bodies.push(body);
            if bodies.len() == 1 && has_stream_options {
                let status = [400_u16, 401, 422, 429]
                    .into_iter()
                    .find(|status| self.first_error.contains(&format!("({status} ")));
                Err(UpstreamFailure {
                    kind: UpstreamFailureKind::HttpStatus,
                    status,
                    headers_received_at: Some(
                        UpstreamResponseMeta::synthetic_success().headers_received_at,
                    ),
                    upstream_request_id: None,
                    client_response: None,
                    retryable: status == Some(429),
                    stable_error_code: status
                        .map(|status| format!("upstream_http_{status}"))
                        .unwrap_or_else(|| "upstream_transport".to_string()),
                    sanitized_summary: self.first_error.clone(),
                })
            } else {
                Ok(UpstreamResponse {
                    meta: UpstreamResponseMeta::synthetic_success(),
                    body: Box::pin(futures::stream::empty()),
                })
            }
        }

        async fn post_json(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            _body: String,
        ) -> Result<String> {
            Err(KeyComputeError::ProviderError("not used".into()))
        }

        async fn post_stream(
            &self,
            _url: &str,
            _headers: Vec<(String, String)>,
            body: String,
        ) -> Result<ByteStream> {
            let mut bodies = self.bodies.lock().unwrap();
            let has_stream_options = body.contains("stream_options");
            bodies.push(body);
            if bodies.len() == 1 && has_stream_options {
                Err(KeyComputeError::ProviderError(self.first_error.clone()))
            } else {
                Ok(Box::pin(futures::stream::empty()))
            }
        }

        fn request_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn stream_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }
    }

    fn stream_request() -> UpstreamRequest {
        UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "gpt-4o")
            .with_message("user", "Hello")
            .with_stream(true)
    }

    #[test]
    fn native_chat_serialization_preserves_supported_fields_and_overlays_routing() {
        let mut request =
            UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "routed-model")
                .with_stream(true);
        request.native_openai_chat_request = Some(std::sync::Arc::new(serde_json::json!({
            "model": "client-model",
            "messages": [
                {"role": "developer", "content": "Be concise"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "weather", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "sunny"}
            ],
            "max_completion_tokens": 321,
            "parallel_tool_calls": false,
            "prompt_cache_key": "cache-user-42",
            "safety_identifier": "safe-user-42",
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "answer", "schema": {"type": "object"}}
            },
            "stream_options": {"include_obfuscation": false},
            "vendor_private": true
        })));

        let body: serde_json::Value =
            serde_json::from_str(&OpenAIProvider::serialize_native_chat_body(&request).unwrap())
                .unwrap();
        assert_eq!(body["model"], "routed-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_completion_tokens"], 321);
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["prompt_cache_key"], "cache-user-42");
        assert_eq!(body["safety_identifier"], "safe-user-42");
        assert_eq!(body["messages"][0]["role"], "developer");
        assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["stream_options"]["include_obfuscation"], false);
        assert_eq!(body["vendor_private"], true);
    }

    #[test]
    fn native_chat_compatibility_retry_removes_stream_options() {
        let mut request = stream_request();
        request.include_stream_usage = false;
        request.native_openai_chat_request = Some(std::sync::Arc::new(serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
            "stream_options": {"include_usage": true}
        })));

        let body: serde_json::Value =
            serde_json::from_str(&OpenAIProvider::serialize_native_chat_body(&request).unwrap())
                .unwrap();
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn native_chat_non_stream_preserves_unowned_stream_options() {
        let mut request = stream_request();
        request.stream = false;
        request.native_openai_chat_request = Some(std::sync::Arc::new(serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false,
            "stream_options": {"future_option": true}
        })));

        let body: serde_json::Value =
            serde_json::from_str(&OpenAIProvider::serialize_native_chat_body(&request).unwrap())
                .unwrap();
        assert_eq!(body["stream_options"]["future_option"], true);
    }

    #[tokio::test]
    async fn native_chat_non_stream_response_preserves_tool_calls_and_usage_details() {
        let transport = NativeResponsesTransport {
            url: Mutex::new(None),
            headers: Mutex::new(Vec::new()),
            body: Mutex::new(None),
            response: serde_json::json!({
                "id": "chatcmpl-native",
                "object": "chat.completion",
                "created": 1,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "weather", "arguments": "{}"}
                        }]
                    },
                    "finish_reason": "tool_calls",
                    "logprobs": null
                }],
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 5,
                    "total_tokens": 17,
                    "prompt_tokens_details": {"cached_tokens": 4}
                }
            })
            .to_string(),
            status: 200,
        };
        let provider = OpenAIProvider::new();
        let mut request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "gpt-4o");
        request.native_openai_chat_request = Some(std::sync::Arc::new(serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "weather"}]
        })));

        let response = provider
            .stream_chat_with_meta(&transport, request)
            .await
            .unwrap();
        let events = response.body.collect::<Vec<_>>().await;
        assert!(matches!(
            &events[0],
            Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiChatJson { body, .. }
            }) if body.pointer("/choices/0/message/tool_calls/0/function/name")
                .and_then(serde_json::Value::as_str) == Some("weather")
                && body.pointer("/usage/prompt_tokens_details/cached_tokens")
                    .and_then(serde_json::Value::as_u64) == Some(4)
        ));
        assert!(matches!(
            events[1],
            Ok(StreamEvent::Usage {
                input_tokens: 12,
                output_tokens: 5
            })
        ));
        assert!(matches!(events[2], Ok(StreamEvent::Done)));
    }

    #[test]
    fn native_chat_non_stream_response_requires_the_core_openai_envelope() {
        let valid = serde_json::json!({
            "id": "chatcmpl-valid",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello"},
                "finish_reason": "stop"
            }]
        });
        assert!(OpenAIProvider::validate_native_chat_success(&valid).is_ok());

        let invalid = [
            serde_json::json!([]),
            serde_json::json!({
                "object": "chat.completion",
                "created": 1,
                "model": "gpt-4o",
                "choices": []
            }),
            serde_json::json!({
                "id": "chatcmpl-invalid",
                "object": "chat.completion",
                "created": "now",
                "model": "gpt-4o",
                "choices": []
            }),
            serde_json::json!({
                "id": "chatcmpl-invalid",
                "object": "chat.completion",
                "created": 1,
                "model": "",
                "choices": []
            }),
            serde_json::json!({
                "id": "chatcmpl-invalid",
                "object": "chat.completion",
                "created": 1,
                "model": "gpt-4o",
                "choices": [{"index": 0, "message": "not-an-object"}]
            }),
        ];
        for response in invalid {
            assert!(OpenAIProvider::validate_native_chat_success(&response).is_err());
        }
    }

    #[test]
    fn native_responses_request_preserves_an_omitted_model() {
        let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "");
        let native = NativeResponsesRequest {
            body: std::sync::Arc::new(serde_json::json!({"input": "hello"})),
            path: "/responses".to_string(),
            headers: std::collections::BTreeMap::new(),
        };

        let body: serde_json::Value = serde_json::from_str(
            &OpenAIProvider::serialize_native_responses_body(&request, &native).unwrap(),
        )
        .unwrap();
        assert!(body.get("model").is_none());
        assert_eq!(body["input"], "hello");
    }

    #[test]
    fn native_compact_serialization_omits_stream_and_overlays_the_routed_model() {
        let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "routed-model");
        let native = NativeResponsesRequest {
            body: std::sync::Arc::new(serde_json::json!({
                "model": "client-model",
                "stream": false,
                "future_field": {"kept": true}
            })),
            path: "/responses/compact".to_string(),
            headers: std::collections::BTreeMap::new(),
        };

        let body: serde_json::Value = serde_json::from_str(
            &OpenAIProvider::serialize_native_responses_body(&request, &native).unwrap(),
        )
        .unwrap();

        assert_eq!(body["model"], "routed-model");
        assert!(body.get("stream").is_none());
        assert_eq!(body["future_field"]["kept"], true);
    }

    #[tokio::test]
    async fn native_responses_request_preserves_unknown_fields_in_typed_json() {
        use futures::StreamExt;

        let transport = NativeResponsesTransport {
            url: Mutex::new(None),
            headers: Mutex::new(Vec::new()),
            body: Mutex::new(None),
            response: serde_json::json!({
                "id": "resp_1",
                "object": "response",
                "status": "completed",
                "output": [],
                "future_response_field": {"kept": true},
                "usage": {"input_tokens": 9, "output_tokens": 4, "total_tokens": 13}
            })
            .to_string(),
            status: 200,
        };
        let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "routed-model");
        let native_request = NativeResponsesRequest {
            body: std::sync::Arc::new(serde_json::json!({
                "model": "client-model",
                "input": "hello",
                "tools": [{"type": "function", "name": "lookup", "parameters": {}}],
                "reasoning": {"effort": "high"},
                "future_request_field": {"kept": true}
            })),
            path: "/responses".to_string(),
            headers: std::collections::BTreeMap::from([
                ("openai-beta".to_string(), "responses=v1".to_string()),
                (
                    "x-client-request-id".to_string(),
                    "trace/opaque.123".to_string(),
                ),
            ]),
        };

        let response = OpenAIProvider::new()
            .stream_responses_with_meta(&transport, request, native_request)
            .await
            .unwrap();
        assert_eq!(
            transport.url.lock().unwrap().as_deref(),
            Some("https://api.openai.com/v1/responses")
        );
        let sent: serde_json::Value =
            serde_json::from_str(transport.body.lock().unwrap().as_deref().unwrap()).unwrap();
        assert_eq!(sent["model"], "routed-model");
        assert_eq!(sent["stream"], false);
        assert_eq!(sent["reasoning"]["effort"], "high");
        assert_eq!(sent["future_request_field"]["kept"], true);
        assert!(
            transport
                .headers
                .lock()
                .unwrap()
                .iter()
                .any(|(name, value)| name == "openai-beta" && value == "responses=v1")
        );
        assert!(
            transport
                .headers
                .lock()
                .unwrap()
                .iter()
                .any(|(name, value)| {
                    name == "x-client-request-id" && value == "trace/opaque.123"
                })
        );

        let events = response.body.collect::<Vec<_>>().await;
        let StreamEvent::Native {
            event: NativeStreamEvent::OpenAiResponsesJson { body, .. },
        } = events[0].as_ref().unwrap()
        else {
            panic!("expected typed response body")
        };
        assert_eq!(body["future_response_field"]["kept"], true);
        assert!(matches!(
            events[1],
            Ok(StreamEvent::Usage {
                input_tokens: 9,
                output_tokens: 4
            })
        ));
        assert!(matches!(events[2], Ok(StreamEvent::Done)));
    }

    #[tokio::test]
    async fn native_responses_allows_terminal_json_without_usage_for_gateway_estimation() {
        use futures::StreamExt;

        let transport = NativeResponsesTransport {
            url: Mutex::new(None),
            headers: Mutex::new(Vec::new()),
            body: Mutex::new(None),
            response: serde_json::json!({
                "id": "resp_without_usage",
                "object": "response",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "content": [{"type": "output_text", "text": "hello"}]
                }],
                "usage": null
            })
            .to_string(),
            status: 200,
        };
        let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "gpt-4o");
        let native_request = NativeResponsesRequest {
            body: std::sync::Arc::new(serde_json::json!({"input": "hello"})),
            path: "/responses".to_string(),
            headers: std::collections::BTreeMap::new(),
        };

        let events = OpenAIProvider::new()
            .stream_responses_with_meta(&transport, request, native_request)
            .await
            .unwrap()
            .body
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            &events[0],
            Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesJson { body, .. }
            }) if body["output"][0]["content"][0]["text"] == "hello"
        ));
        assert!(matches!(events[1], Ok(StreamEvent::Done)));
        assert_eq!(events.len(), 2, "missing usage must not be marked exact");
    }

    #[tokio::test]
    async fn native_responses_rejects_malformed_success_objects() {
        for response in [
            serde_json::json!({}),
            serde_json::json!({
                "error": {
                    "type": "invalid_request_error",
                    "message": "bad request"
                }
            }),
            serde_json::json!({
                "id": "",
                "object": "response",
                "status": "completed",
                "output": []
            }),
            serde_json::json!({
                "id": "resp_unknown_status",
                "object": "response",
                "status": "future_status",
                "output": []
            }),
        ] {
            let transport = NativeResponsesTransport {
                url: Mutex::new(None),
                headers: Mutex::new(Vec::new()),
                body: Mutex::new(None),
                response: response.to_string(),
                status: 200,
            };
            let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "gpt-4o");
            let native_request = NativeResponsesRequest {
                body: std::sync::Arc::new(serde_json::json!({"input": "hello"})),
                path: "/responses".to_string(),
                headers: std::collections::BTreeMap::new(),
            };

            let error = match OpenAIProvider::new()
                .stream_responses_with_meta(&transport, request, native_request)
                .await
            {
                Ok(_) => panic!("malformed 2xx Responses body must be rejected"),
                Err(error) => error,
            };
            assert_eq!(error.kind, UpstreamFailureKind::Protocol);
            assert_eq!(error.status, Some(200));
        }
    }

    #[tokio::test]
    async fn native_compact_accepts_compacted_response_shape() {
        let transport = NativeResponsesTransport {
            url: Mutex::new(None),
            headers: Mutex::new(Vec::new()),
            body: Mutex::new(None),
            response: serde_json::json!({
                "id": "resp_compact_1",
                "object": "response.compaction",
                "created_at": 1,
                "output": [],
                "usage": {"input_tokens": 9, "output_tokens": 4, "total_tokens": 13}
            })
            .to_string(),
            status: 200,
        };
        let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "gpt-4o");
        let native_request = NativeResponsesRequest {
            body: std::sync::Arc::new(serde_json::json!({"input": "hello"})),
            path: "/responses/compact".to_string(),
            headers: std::collections::BTreeMap::new(),
        };

        let events = OpenAIProvider::new()
            .stream_responses_with_meta(&transport, request, native_request)
            .await
            .unwrap()
            .body
            .collect::<Vec<_>>()
            .await;

        assert!(matches!(
            &events[0],
            Ok(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesJson { body, .. }
            }) if body["object"] == "response.compaction"
        ));
        assert!(matches!(events[2], Ok(StreamEvent::Done)));
    }

    #[tokio::test]
    async fn native_responses_preserves_non_success_status_body_and_safe_headers() {
        let body = serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "code": "bad_model",
                "message": "bad model"
            }
        })
        .to_string();
        let transport = NativeResponsesTransport {
            url: Mutex::new(None),
            headers: Mutex::new(Vec::new()),
            body: Mutex::new(None),
            response: body.clone(),
            status: 429,
        };
        let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "model");
        let native_request = NativeResponsesRequest {
            body: std::sync::Arc::new(serde_json::json!({"model": "model", "input": "hi"})),
            path: "/responses".to_string(),
            headers: std::collections::BTreeMap::new(),
        };

        let mut response = OpenAIProvider::new()
            .stream_responses_with_meta(&transport, request, native_request)
            .await
            .unwrap();
        assert_eq!(response.meta.status, 429);
        let StreamEvent::Native {
            event:
                NativeStreamEvent::OpenAiResponsesHttpError {
                    status,
                    headers,
                    body: actual_body,
                },
        } = response.body.next().await.unwrap().unwrap()
        else {
            panic!("expected typed error event");
        };
        assert_eq!(status, 429);
        assert_eq!(actual_body, body);
        assert_eq!(headers[0].0, "content-type");
        assert_eq!(headers.len(), 1);
    }

    #[tokio::test]
    async fn streamed_responses_error_body_has_a_hard_size_limit() {
        let mut meta = UpstreamResponseMeta::synthetic_success();
        meta.status = 502;
        meta.upstream_request_id = Some("upstream-request".to_string());
        let stream: ByteStream = Box::pin(futures::stream::iter([
            Ok(bytes::Bytes::from_static(b"1234")),
            Ok(bytes::Bytes::from_static(b"56")),
        ]));

        let error = OpenAIProvider::collect_native_responses_error_body(stream, &meta, 5)
            .await
            .unwrap_err();
        assert_eq!(error.kind, UpstreamFailureKind::BodyRead);
        assert_eq!(error.status, Some(502));
        assert!(error.retryable);
        assert_eq!(error.stable_error_code, "upstream_body_too_large");
        assert_eq!(
            error.upstream_request_id.as_deref(),
            Some("upstream-request")
        );
        assert!(error.sanitized_summary.contains("5-byte limit"));
    }

    #[tokio::test]
    async fn non_streamed_responses_error_body_has_a_hard_size_limit() {
        let transport = NativeResponsesTransport {
            url: Mutex::new(None),
            headers: Mutex::new(Vec::new()),
            body: Mutex::new(None),
            response: "x".repeat(MAX_JSON_PASSTHROUGH_ERROR_BODY_BYTES + 1),
            status: 502,
        };
        let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "model");
        let native_request = NativeResponsesRequest {
            body: std::sync::Arc::new(serde_json::json!({"model": "model", "input": "hi"})),
            path: "/responses".to_string(),
            headers: std::collections::BTreeMap::new(),
        };

        let error = match OpenAIProvider::new()
            .stream_responses_with_meta(&transport, request, native_request)
            .await
        {
            Ok(_) => panic!("oversized error response must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind, UpstreamFailureKind::BodyRead);
        assert_eq!(error.status, Some(502));
        assert!(error.retryable);
        assert_eq!(error.stable_error_code, "upstream_body_too_large");
        assert!(error.sanitized_summary.contains("1048576-byte limit"));
    }

    #[test]
    fn non_streamed_responses_reject_dense_json_before_tree_allocation() {
        let meta = UpstreamResponseMeta::synthetic_success();
        let body = r#"{"object":"response","id":"resp_1","output":[0,0,0,0],"status":"completed"}"#;
        let estimated = estimated_json_parse_working_set_bytes(body.as_bytes());

        let error =
            OpenAIProvider::validate_native_responses_json_working_set(&meta, body, estimated - 1)
                .expect_err(
                    "a response over the working-set limit must be rejected before parsing",
                );

        assert_eq!(error.kind, UpstreamFailureKind::BodyRead);
        assert_eq!(error.stable_error_code, "upstream_json_too_complex");
        assert!(!error.retryable);
        assert!(error.sanitized_summary.contains("working-set limit"));
    }

    #[tokio::test]
    async fn stream_options_400_requests_an_executor_tracked_retry() {
        let provider = OpenAIProvider::new();
        let transport =
            DegradingTransport::new("HTTP error (400 Bad Request): unknown field stream_options");

        let error = match provider
            .stream_chat_with_meta(&transport, stream_request())
            .await
        {
            Ok(_) => panic!("adapter must not hide the compatibility retry"),
            Err(error) => error,
        };
        assert_eq!(
            error.stable_error_code,
            "upstream_stream_options_unsupported"
        );
        assert!(error.retryable);

        let bodies = transport.bodies.lock().unwrap();
        assert_eq!(
            bodies.len(),
            1,
            "adapter must issue exactly one HTTP request"
        );
        assert!(
            bodies[0].contains("stream_options"),
            "first request should carry stream_options"
        );
    }

    #[tokio::test]
    async fn stream_options_422_requests_an_executor_tracked_retry() {
        let provider = OpenAIProvider::new();
        let transport = DegradingTransport::new(
            "HTTP error (422 Unprocessable Entity): extra field stream_options",
        );

        let error = match provider
            .stream_chat_with_meta(&transport, stream_request())
            .await
        {
            Ok(_) => panic!("adapter must expose the first HTTP failure"),
            Err(error) => error,
        };
        assert_eq!(
            error.stable_error_code,
            "upstream_stream_options_unsupported"
        );
        assert_eq!(transport.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn executor_can_retry_without_stream_options() {
        let provider = OpenAIProvider::new();
        let transport =
            DegradingTransport::new("HTTP error (400 Bad Request): unknown field stream_options");
        let mut request = stream_request();
        request.include_stream_usage = false;

        let result = provider.stream_chat_with_meta(&transport, request).await;
        assert!(result.is_ok());
        let bodies = transport.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert!(!bodies[0].contains("stream_options"));
    }

    #[tokio::test]
    async fn test_stream_options_no_retry_on_auth_or_ratelimit() {
        // 401/429 与请求体无关，不应触发降级重试
        for error in [
            "HTTP error (401 Unauthorized): invalid api key",
            "HTTP error (429 Too Many Requests): rate limited",
        ] {
            let provider = OpenAIProvider::new();
            let transport = DegradingTransport::new(error);

            let result = provider.stream_chat(&transport, stream_request()).await;
            assert!(result.is_err(), "{error} should not be retried");
            assert_eq!(
                transport.bodies.lock().unwrap().len(),
                1,
                "{error} should only be sent once"
            );
        }
    }

    #[tokio::test]
    async fn stream_options_is_not_removed_for_an_unrelated_client_error() {
        let transport = DegradingTransport::new("HTTP error (400 Bad Request): invalid model");
        let provider = OpenAIProvider::new();

        let error = match provider
            .stream_chat_with_meta(&transport, stream_request())
            .await
        {
            Ok(_) => panic!("unrelated 400 should not trigger compatibility retry"),
            Err(error) => error,
        };

        assert_eq!(error.status, Some(400));
        assert_ne!(
            error.stable_error_code,
            "upstream_stream_options_unsupported"
        );
        assert_eq!(transport.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn structured_http_failure_preserves_status_and_upstream_request_id() {
        let provider = OpenAIProvider::new();
        let transport = MetadataTransport { fail_stream: true };
        let error = match provider
            .stream_chat_with_meta(&transport, stream_request())
            .await
        {
            Ok(_) => panic!("429 response must fail"),
            Err(error) => error,
        };
        assert_eq!(error.status, Some(429));
        assert_eq!(
            error.upstream_request_id.as_deref(),
            Some("upstream-rate-id")
        );
        assert_eq!(error.stable_error_code, "upstream_http_429");
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn protocol_failure_keeps_response_metadata_separate_from_first_content() {
        let provider = OpenAIProvider::new();
        let transport = MetadataTransport { fail_stream: false };
        let request = UpstreamRequest::new("https://provider.example/v1", "sk-test", "gpt-test")
            .with_message("user", "hello");
        let error = match provider.stream_chat_with_meta(&transport, request).await {
            Ok(_) => panic!("invalid JSON must fail before producing protocol content"),
            Err(error) => error,
        };
        assert_eq!(error.kind, UpstreamFailureKind::Protocol);
        assert_eq!(error.status, Some(200));
        assert!(error.headers_received_at.is_some());
        assert_eq!(
            error.upstream_request_id.as_deref(),
            Some("upstream-json-id")
        );
    }

    #[test]
    fn test_openai_provider_name() {
        let provider = OpenAIProvider::new();
        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn test_openai_supported_models_empty() {
        let provider = OpenAIProvider::new();
        // 协议层不维护模型白名单
        assert!(provider.supported_models().is_empty());
    }

    #[test]
    fn test_openai_supports_any_model() {
        let provider = OpenAIProvider::new();
        assert!(provider.supports_model("gpt-4o"));
        assert!(provider.supports_model("deepseek-chat"));
        assert!(provider.supports_model("any-model"));
    }

    #[test]
    fn test_chat_url_join() {
        assert_eq!(
            OpenAIProvider::chat_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            OpenAIProvider::chat_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        // 空 endpoint 回落的协议默认 Base URL 必须能拼出完整请求 URL
        assert_eq!(
            OpenAIProvider::chat_url(
                llm_protocol_provider::ProtocolType::Openai.default_endpoint()
            ),
            OPENAI_CHAT_ENDPOINT
        );
    }

    #[test]
    fn test_build_request_body() {
        let provider = OpenAIProvider::new();
        let request = UpstreamRequest::new("https://api.openai.com/v1", "sk-test", "gpt-4o")
            .with_message("system", "You are helpful")
            .with_message("user", "Hello")
            .with_stream(true)
            .with_max_tokens(100)
            .with_temperature(0.7);

        let body = provider.build_request_body(&request);

        assert_eq!(body.model, "gpt-4o");
        assert_eq!(body.messages.len(), 2);
        assert_eq!(body.stream, Some(true));
        assert_eq!(body.max_tokens, Some(100));
        assert_eq!(body.temperature, Some(0.7));
    }
}
