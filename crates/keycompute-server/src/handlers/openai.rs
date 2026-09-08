//! OpenAI 兼容 API 处理器
//
//! 提供 OpenAI API 的常用关键字段与原生响应语义
//! 参考: https://platform.openai.com/docs/api-reference

#[cfg(test)]
use super::ImmediateSettlementServices;
use crate::{
    error::{ApiError, Result},
    extractors::{AuthExtractor, ClientRequestId, RequestId, RequestReceivedAt},
    state::AppState,
};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query, State},
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
};
use futures::{StreamExt, stream::Stream};
use keycompute_auth::Permission;
use keycompute_db::models::account::Account;
use keycompute_types::{
    AccountApiCapability, ClientResponseOutcome, ContentPart, ErrorOrigin, ExecutionTarget,
    ImageUrl, Message, MessageContent, MessageRole, NoopRequestLifecycleRecorder, RequestContext,
    RequestLifecycleRecorder, RequestStatus, RequestTraceStart, RouteType, TraceErrorCategory,
};
use llm_protocol_provider::{
    LARGE_NATIVE_EVENT_CHANNEL_CAPACITY, LargeBodyPermit, MAX_JSON_PASSTHROUGH_BODY_BYTES,
    MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES, NativeStreamEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

// ==================== Chat Completions ====================

/// Chat supports inline images, audio and files. Match the bounded native JSON
/// passthrough allowance used by the provider layer instead of Axum's 2 MiB
/// default.
pub const OPENAI_CHAT_BODY_LIMIT_BYTES: usize = MAX_JSON_PASSTHROUGH_BODY_BYTES;
pub const OPENAI_CHAT_REQUEST_WORKING_SET_LIMIT_BYTES: usize =
    MAX_JSON_PASSTHROUGH_WORKING_SET_BYTES;

/// Chat Completions 路由投影。
///
/// 这里只反序列化路由、计费和本地校验实际需要的字段。完整 JSON 由
/// `RequestContext::native_openai_chat_request` 保留并交给原生 OpenAI
/// adapter；新增官方字段、自定义工具和新的内容块类型因此无需等待网关
/// DTO 升级即可无损转发。
#[derive(Debug)]
pub struct ChatCompletionRequest {
    /// 模型 ID (必需)
    pub model: String,
    /// 消息列表 (必需)
    pub messages: Vec<ChatRoutingMessage>,
    /// 是否流式输出 (默认 false)
    pub stream: bool,
    /// 最大生成 token 数
    pub max_tokens: Option<u32>,
    /// 最大生成 token 数（OpenAI 新版字段，与 max_tokens 等效；
    /// 两者同时提供时 max_tokens 优先）
    pub max_completion_tokens: Option<u32>,
    /// 温度参数 (0-2)
    pub temperature: Option<f32>,
    /// 核采样参数 (0-1)
    pub top_p: Option<f32>,
    /// 每个提示生成的结果数 (默认 1)
    pub n: Option<u32>,
    /// 是否返回输入 token 的用量
    pub stream_options: Option<StreamOptions>,
    /// 存在惩罚 (-2.0 到 2.0)
    pub presence_penalty: Option<f32>,
    /// 频率惩罚 (-2.0 到 2.0)
    pub frequency_penalty: Option<f32>,
    /// 日志概率 (0-5)
    pub logprobs: Option<bool>,
    /// 返回的日志概率选项数
    pub top_logprobs: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ChatRoutingMessage {
    pub role: String,
    pub content: MessageContent,
}

impl<'de> Deserialize<'de> for ChatCompletionRequest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let body = Value::deserialize(deserializer)?;
        parse_chat_completion_request(&body).map_err(serde::de::Error::custom)
    }
}

impl ChatCompletionRequest {
    /// 内部路由/估算使用的最大生成 token 数；原生上游请求仍分别保留两字段。
    fn effective_max_tokens(&self) -> Option<u32> {
        self.max_tokens.or(self.max_completion_tokens)
    }

    /// 校验采样参数范围
    ///
    /// 越界参数在 handler 层直接返回 400，避免确定性的上游 400
    /// 级联整条 fallback 链（浪费上游调用）并污染 Provider 健康评分。
    /// 注：NaN 不在任何区间内，同样会被拒绝
    fn validate_sampling_params(&self) -> Result<()> {
        if self.max_tokens == Some(0) {
            return Err(ApiError::BadRequest(
                "max_tokens must be greater than 0".to_string(),
            ));
        }
        if self.max_completion_tokens == Some(0) {
            return Err(ApiError::BadRequest(
                "max_completion_tokens must be greater than 0".to_string(),
            ));
        }
        if let Some(temperature) = self.temperature
            && !(0.0..=2.0).contains(&temperature)
        {
            return Err(ApiError::BadRequest(
                "temperature must be between 0.0 and 2.0".to_string(),
            ));
        }
        if let Some(top_p) = self.top_p
            && !(0.0..=1.0).contains(&top_p)
        {
            return Err(ApiError::BadRequest(
                "top_p must be between 0.0 and 1.0".to_string(),
            ));
        }
        if let Some(presence_penalty) = self.presence_penalty
            && !(-2.0..=2.0).contains(&presence_penalty)
        {
            return Err(ApiError::BadRequest(
                "presence_penalty must be between -2.0 and 2.0".to_string(),
            ));
        }
        if let Some(frequency_penalty) = self.frequency_penalty
            && !(-2.0..=2.0).contains(&frequency_penalty)
        {
            return Err(ApiError::BadRequest(
                "frequency_penalty must be between -2.0 and 2.0".to_string(),
            ));
        }
        if self.n == Some(0) {
            return Err(ApiError::BadRequest("n must be greater than 0".to_string()));
        }
        if self.top_logprobs.is_some() && self.logprobs != Some(true) {
            return Err(ApiError::BadRequest(
                "logprobs must be true when top_logprobs is specified".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_core_fields(&self) -> Result<()> {
        if self.model.trim().is_empty() {
            return Err(ApiError::BadRequest("model must not be empty".to_string()));
        }
        if self.messages.is_empty() {
            return Err(ApiError::BadRequest(
                "messages must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

fn parse_chat_completion_request(body: &Value) -> Result<ChatCompletionRequest> {
    let object = body.as_object().ok_or_else(|| {
        ApiError::BadRequest("Chat Completions request body must be a JSON object".to_string())
    })?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("model must be a string".to_string()))?
        .to_string();
    let raw_messages = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::BadRequest("messages must be an array".to_string()))?;
    let messages = raw_messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let message = message.as_object().ok_or_else(|| {
                ApiError::BadRequest(format!("messages[{index}] must be an object"))
            })?;
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ApiError::BadRequest(format!("messages[{index}].role must be a string"))
                })?
                .to_string();
            Ok(ChatRoutingMessage {
                role,
                content: project_chat_message_content(message.get("content")),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let stream = match object.get("stream") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(ApiError::BadRequest(
                "stream must be a boolean or null".to_string(),
            ));
        }
    };

    Ok(ChatCompletionRequest {
        model,
        messages,
        stream,
        max_tokens: optional_chat_field(object, "max_tokens")?,
        max_completion_tokens: optional_chat_field(object, "max_completion_tokens")?,
        temperature: optional_chat_field(object, "temperature")?,
        top_p: optional_chat_field(object, "top_p")?,
        n: if object.contains_key("n") {
            optional_chat_field(object, "n")?
        } else {
            Some(1)
        },
        stream_options: project_chat_stream_options(object.get("stream_options"))?,
        presence_penalty: optional_chat_field(object, "presence_penalty")?,
        frequency_penalty: optional_chat_field(object, "frequency_penalty")?,
        logprobs: optional_chat_field(object, "logprobs")?,
        top_logprobs: optional_chat_field(object, "top_logprobs")?,
    })
}

fn project_chat_stream_options(value: Option<&Value>) -> Result<Option<StreamOptions>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or_else(|| {
        ApiError::BadRequest("stream_options must be an object or null".to_string())
    })?;
    let include_usage = match object.get("include_usage") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(ApiError::BadRequest(
                "stream_options.include_usage must be a boolean or null".to_string(),
            ));
        }
    };
    Ok(Some(StreamOptions { include_usage }))
}

fn optional_chat_field<T: serde::de::DeserializeOwned>(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<T>> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| ApiError::BadRequest(format!("invalid {field}: {error}")))
}

/// Chat Completion 消息
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatCompletionMessage {
    /// 角色: system, user, assistant, tool
    pub role: String,
    /// 原生内容。OpenAI 可在这里增加 text/image/audio/file/refusal 等块；
    /// 路由投影不得用封闭枚举拒绝它们。
    pub content: Option<Value>,
    /// 工具调用 (assistant 消息中)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    /// 工具调用 ID (tool 消息中)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 名称 (function 消息中)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 工具调用
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCall {
    /// 调用 ID
    pub id: String,
    /// 调用类型
    #[serde(rename = "type")]
    pub call_type: String,
    /// 函数调用
    pub function: FunctionCall,
}

/// 函数调用
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FunctionCall {
    /// 函数名称
    pub name: String,
    /// 参数 (JSON 字符串)
    pub arguments: String,
}

/// 流式选项
#[derive(Debug, Deserialize)]
pub struct StreamOptions {
    /// 在流式消息的最后包含用量信息
    #[serde(default)]
    pub include_usage: bool,
}

/// 将原生 Chat 内容投影为通用路由/估算表示。
///
/// 原始内容始终由 native body 转发；这里的降维不会改变发往 Provider 的
/// payload。无法表示的官方多模态块使用稳定占位文本，确保 token 估算不会
/// 把 audio/file 等输入当成空消息。
fn project_chat_message_content(content: Option<&Value>) -> MessageContent {
    match content {
        Some(Value::String(text)) => MessageContent::text(text.clone()),
        Some(Value::Array(parts)) => {
            let mut projected = Vec::new();
            for part in parts {
                let Some(object) = part.as_object() else {
                    projected.push(ContentPart::Text {
                        text: part.to_string(),
                    });
                    continue;
                };
                match object.get("type").and_then(Value::as_str) {
                    Some("text" | "input_text") => {
                        if let Some(text) = object.get("text").and_then(Value::as_str) {
                            projected.push(ContentPart::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                    Some("image_url") => {
                        projected.push(project_chat_image_url(object.get("image_url")))
                    }
                    Some(kind) => projected.push(ContentPart::Text {
                        text: format!("[{kind}]"),
                    }),
                    None => projected.push(ContentPart::Text {
                        text: part.to_string(),
                    }),
                }
            }
            if projected.is_empty() {
                MessageContent::text(String::new())
            } else {
                MessageContent::Parts(projected)
            }
        }
        Some(Value::Null) | None => MessageContent::text(String::new()),
        Some(other) => MessageContent::text(other.to_string()),
    }
}

fn project_chat_image_url(value: Option<&Value>) -> ContentPart {
    let (url, detail) = match value {
        Some(Value::String(url)) => (Some(url.as_str()), None),
        Some(Value::Object(image)) => (
            image.get("url").and_then(Value::as_str),
            image.get("detail").and_then(Value::as_str),
        ),
        _ => (None, None),
    };
    let Some(url) = url else {
        return ContentPart::Text {
            text: "[image_url]".to_string(),
        };
    };
    // Inline base64 and pathological URLs stay only in the native body. The
    // projection needs only enough information to select keepalive behavior.
    if url.starts_with("data:") || url.len() > 8 * 1024 {
        return ContentPart::Text {
            text: "[inline_image]".to_string(),
        };
    }
    ContentPart::ImageUrl {
        image_url: ImageUrl {
            url: url.to_string(),
            detail: detail.map(str::to_string),
        },
    }
}

/// Roles are only a routing/token-estimation projection. Preserve the native
/// role in the forwarded JSON and avoid making this gateway the compatibility
/// bottleneck when OpenAI adds a role. The legacy `function` role is closest
/// to the common tool role; unknown future roles use a neutral user projection.
fn project_chat_message_role(role: &str) -> MessageRole {
    match role {
        "system" => MessageRole::System,
        "developer" => MessageRole::Developer,
        "assistant" => MessageRole::Assistant,
        "tool" | "function" => MessageRole::Tool,
        _ => MessageRole::User,
    }
}

/// Rehydrate the subset of native Chat messages that the Node task protocol
/// can represent. Provider routing continues to use the lightweight
/// projection, while Node routing must retain supported inline image bytes.
fn node_chat_messages(body: &Value) -> Result<Vec<Message>> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::BadRequest("messages must be an array".to_string()))?;

    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let message = message.as_object().ok_or_else(|| {
                ApiError::BadRequest(format!("messages[{index}] must be an object"))
            })?;
            let role = match message.get("role").and_then(Value::as_str) {
                Some("system") => MessageRole::System,
                Some("developer") => MessageRole::Developer,
                Some("user") => MessageRole::User,
                Some("assistant") => MessageRole::Assistant,
                Some("tool") => MessageRole::Tool,
                Some(role) => {
                    return Err(ApiError::BadRequest(format!(
                        "messages[{index}].role {role:?} is not supported by Node Chat tasks"
                    )));
                }
                None => {
                    return Err(ApiError::BadRequest(format!(
                        "messages[{index}].role must be a string"
                    )));
                }
            };
            let content = match message.get("content") {
                None | Some(Value::Null) => MessageContent::text(String::new()),
                Some(content) => serde_json::from_value(content.clone()).map_err(|error| {
                    ApiError::BadRequest(format!(
                        "messages[{index}].content is not supported by Node Chat tasks: {error}"
                    ))
                })?,
            };
            Ok(Message { role, content })
        })
        .collect()
}

/// Chat Completion 响应 (非流式)
#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    /// 响应 ID
    pub id: String,
    /// 对象类型: chat.completion
    pub object: String,
    /// 创建时间戳 (Unix)
    pub created: i64,
    /// 模型名称
    pub model: String,
    /// 选择列表
    pub choices: Vec<ChatCompletionChoice>,
    /// 用量信息
    pub usage: CompletionUsage,
    /// 系统指纹
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
}

/// Chat Completion 选择项
#[derive(Debug, Serialize)]
pub struct ChatCompletionChoice {
    /// 索引
    pub index: u32,
    /// 消息
    pub message: ChatCompletionMessage,
    /// 结束原因: stop, length, content_filter, tool_calls
    pub finish_reason: Option<String>,
    /// 日志概率信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// 用量信息
#[derive(Debug, Serialize)]
pub struct CompletionUsage {
    /// 输入 token 数
    pub prompt_tokens: u32,
    /// 输出 token 数
    pub completion_tokens: u32,
    /// 总 token 数
    pub total_tokens: u32,
    /// 详细 token 信息 (可选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<TokenDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<TokenDetails>,
}

/// Token 详情
#[derive(Debug, Serialize)]
pub struct TokenDetails {
    /// 缓存的 token 数
    pub cached_tokens: Option<u32>,
    /// 音频 token 数
    pub audio_tokens: Option<u32>,
}

/// Chat Completion 流式响应块
#[derive(Debug, Serialize)]
pub struct ChatCompletionChunk {
    /// 响应 ID
    pub id: String,
    /// 对象类型: chat.completion.chunk
    pub object: String,
    /// 创建时间戳
    pub created: i64,
    /// 模型名称
    pub model: String,
    /// 系统指纹
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
    /// 选择列表
    pub choices: Vec<ChatCompletionChunkChoice>,
    /// 用量信息 (仅在最后一块，如果 stream_options.include_usage 为 true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<CompletionUsage>,
}

/// Chat Completion 流式选择项
#[derive(Debug, Serialize)]
pub struct ChatCompletionChunkChoice {
    /// 索引
    pub index: u32,
    /// Delta 内容
    pub delta: ChatCompletionChunkDelta,
    /// 结束原因
    pub finish_reason: Option<String>,
    /// 日志概率
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// Delta 内容
#[derive(Debug, Serialize, Default)]
pub struct ChatCompletionChunkDelta {
    /// 角色 (仅第一条)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// 内容
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 工具调用
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

async fn finish_unexecuted_trace(
    guard: &mut super::PreExecutionTraceGuard,
    origin: ErrorOrigin,
    category: TraceErrorCategory,
    code: &str,
) {
    guard.finish_failed(origin, category, code).await;
}

/// Tie large-request admission to detached work that still owns the decoded
/// request or a derived payload after the HTTP handler can be cancelled.
async fn retain_generation_body_permit<F>(
    permit: Option<crate::state::GenerationHttpBodyPermit>,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    let _permit = permit;
    future.await
}

/// Chat Completions 处理器
/// POST /v1/chat/completions
///
/// 注意：限流已在中间件层统一处理，此处直接开始业务逻辑
pub async fn chat_completions(
    State(state): State<AppState>,
    auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received_at: RequestReceivedAt,
    (body_permit, Json(body)): (
        Option<Extension<crate::state::GenerationHttpBodyPermit>>,
        Json<Value>,
    ),
) -> Result<axum::response::Response> {
    let request = parse_chat_completion_request(&body)?;
    let native_chat_request = Arc::new(body);
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
            request_path: "/v1/chat/completions".to_string(),
            requested_model: request.model.clone(),
            is_stream: request.stream,
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
        finish_unexecuted_trace(
            &mut pre_execution_guard,
            ErrorOrigin::Client,
            TraceErrorCategory::Authorization,
            "permission_denied",
        )
        .await;
        return Err(ApiError::Forbidden(
            "API-use permission is required for /v1/chat/completions".to_string(),
        ));
    }
    if let Err(error) = request
        .validate_core_fields()
        .and_then(|_| request.validate_sampling_params())
    {
        finish_unexecuted_trace(
            &mut pre_execution_guard,
            ErrorOrigin::Client,
            TraceErrorCategory::InvalidRequest,
            "invalid_chat_parameters",
        )
        .await;
        return Err(error);
    }
    // 1. 构建 PricingSnapshot
    // 注意：此时 provider 尚未确定（路由在之后执行）
    // Node 模型（node:前缀）使用 empty provider，其他使用 openai
    let provider = keycompute_pricing::resolve_pricing_provider(&request.model);
    let pricing = match state
        .pricing
        .create_snapshot(&request.model, &auth.tenant_id, Some(provider))
        .await
    {
        Ok(pricing) => pricing,
        Err(error) => {
            finish_unexecuted_trace(
                &mut pre_execution_guard,
                ErrorOrigin::Gateway,
                TraceErrorCategory::Internal,
                "pricing_failed",
            )
            .await;
            return Err(ApiError::Internal(format!(
                "Failed to create pricing snapshot: {}",
                error
            )));
        }
    };

    // 3. 转换消息格式
    let messages: Vec<Message> = request
        .messages
        .iter()
        .map(|m| Message {
            role: project_chat_message_role(&m.role),
            content: m.content.clone(),
        })
        .collect();

    // 4. 构建 RequestContext
    let mut request_ctx = RequestContext::new(
        request_id.0,
        auth.user_id,
        auth.tenant_id,
        auth.produce_ai_key_id,
        request.model.clone(),
        messages,
        request.stream,
        pricing,
    );
    // 透传客户端采样参数（协议层构建上游请求时使用，
    // Anthropic 协议的 max_tokens 为必填字段，不透传会被默认值硬截断）
    request_ctx.max_tokens = request.effective_max_tokens();
    request_ctx.temperature = request.temperature;
    request_ctx.top_p = request.top_p;
    request_ctx.native_openai_chat_request = Some(native_chat_request);
    let mut ctx = Arc::new(request_ctx);

    // 5. 智能路由
    let plan = match state.routing.route(&ctx).await {
        Ok(plan) => plan,
        Err(error) => {
            finish_unexecuted_trace(
                &mut pre_execution_guard,
                ErrorOrigin::Gateway,
                TraceErrorCategory::Internal,
                "routing_failed",
            )
            .await;
            return Err(crate::error::map_routing_error(error, "openai"));
        }
    };
    let (route_type, route_status) = initial_route_trace_state(&plan.primary);
    if let Err(error) = lifecycle
        .set_route(request_id.0, route_type, route_status)
        .await
    {
        tracing::warn!(request_id=%request_id.0, %error, "failed to record request route");
    }

    // 5. 根据 ExecutionTarget 分流执行路径
    match &plan.primary {
        ExecutionTarget::Node { model } => {
            // 更新 ctx 的 model 字段（使用去掉前缀的实际模型名）
            let ctx_mut = Arc::make_mut(&mut ctx);
            ctx_mut.model = model.clone();

            // 更新定价快照（使用实际模型名和 NODE_PRICING_PROVIDER 进行定价查找）
            // 注意：必须先调用 update_context_pricing，再设置 provider
            // 因为 update_context_pricing 会检查 provider 是否变化
            state
                .pricing
                .update_context_pricing(ctx_mut, keycompute_pricing::NODE_PRICING_PROVIDER)
                .await;

            // 设置 provider 字段（用于日志追踪和后续逻辑）
            ctx_mut.set_provider(keycompute_pricing::NODE_PRICING_PROVIDER);

            // 调用 node-gateway 执行
            let Some(node_gateway) = state.node_gateway.as_ref() else {
                finish_unexecuted_trace(
                    &mut pre_execution_guard,
                    ErrorOrigin::Gateway,
                    TraceErrorCategory::Internal,
                    "node_gateway_unavailable",
                )
                .await;
                return Err(ApiError::Internal(
                    "node gateway not configured".to_string(),
                ));
            };

            let node_messages = match ctx
                .native_openai_chat_request
                .as_deref()
                .ok_or_else(|| {
                    ApiError::Internal("native Chat request missing for Node route".to_string())
                })
                .and_then(node_chat_messages)
            {
                Ok(messages) => messages,
                Err(error) => {
                    finish_unexecuted_trace(
                        &mut pre_execution_guard,
                        ErrorOrigin::Client,
                        TraceErrorCategory::InvalidRequest,
                        "unsupported_node_chat_payload",
                    )
                    .await;
                    return Err(error);
                }
            };

            // 构建 NodeTaskPayload
            let payload = keycompute_types::node::NodeTaskPayload {
                request_id: ctx.request_id,
                chat: Some(keycompute_types::ChatCompletionRequest {
                    model: model.clone(), // 使用去掉 node: 前缀的实际模型名
                    messages: node_messages,
                    stream: Some(request.stream), // 传递 stream 标志
                    max_tokens: request.effective_max_tokens(),
                    temperature: request.temperature,
                    top_p: request.top_p,
                    n: request.n,
                    stop: None,
                }),
                image_generation: None,
                image_edit: None,
            };

            // 防御性校验 payload 互斥性
            if let Err(e) = payload.validate() {
                finish_unexecuted_trace(
                    &mut pre_execution_guard,
                    ErrorOrigin::Gateway,
                    TraceErrorCategory::Internal,
                    "invalid_node_task_payload",
                )
                .await;
                return Err(ApiError::Internal(format!(
                    "Invalid NodeTaskPayload: {}",
                    e
                )));
            }

            let balance_reservation = match super::reserve_generation_balance(
                &state,
                &ctx,
                super::GenerationBalanceReservationLifetime::Node(node_gateway.task_deadline()),
            )
            .await
            {
                Ok(reservation) => reservation,
                Err(error) => {
                    finish_unexecuted_trace(
                        &mut pre_execution_guard,
                        ErrorOrigin::Client,
                        TraceErrorCategory::Balance,
                        "balance_reservation_failed",
                    )
                    .await;
                    return Err(error);
                }
            };

            let mut client_response_guard =
                super::ClientResponseGuard::new(Arc::clone(&lifecycle), Arc::clone(&ctx));
            pre_execution_guard.disarm();
            let settlement = super::ImmediateSettlementServices::from_state(&state);

            // Once the task has been enqueued, its usage can arrive after the
            // HTTP handler is cancelled. A detached owner therefore waits for
            // the durable Node task and settles or releases the reservation;
            // the handler only owns delivery of the completed result.
            let worker_node_gateway = Arc::clone(node_gateway);
            let worker_ctx = Arc::clone(&ctx);
            let worker_body_permit = body_permit.map(|Extension(permit)| permit);
            let mut worker_balance_reservation = balance_reservation;
            let node_user_id = auth.user_id;
            let node_model = model.clone();
            let (node_result_tx, node_result_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(retain_generation_body_permit(
                worker_body_permit,
                async move {
                    let result = worker_node_gateway
                        .enqueue_and_wait(node_user_id, node_model, payload)
                        .await;
                    match &result {
                        Ok(response) => {
                            worker_balance_reservation.transfer_to_settlement();
                            worker_ctx.set_input_tokens(response.usage.prompt_tokens);
                            worker_ctx.add_output_tokens(response.usage.completion_tokens);
                            finalize_openai_billing(
                                &settlement,
                                &worker_ctx,
                                keycompute_pricing::NODE_PRICING_PROVIDER,
                                uuid::Uuid::nil(),
                                "success",
                            )
                            .await;
                        }
                        Err(_) => {
                            worker_balance_reservation.release().await;
                        }
                    }
                    let _ = node_result_tx.send(result);
                },
            ));
            let response = match node_result_rx.await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    let outcome = error.client_response_outcome();
                    ctx.set_execution_failure(error.request_failure());
                    client_response_guard.finish_with_outcome(outcome).await;
                    return Err(ApiError::from(error));
                }
                Err(_) => {
                    client_response_guard
                        .finish_with_outcome(ClientResponseOutcome::ResponseFailed)
                        .await;
                    return Err(ApiError::Internal(
                        "Node settlement worker stopped before returning a result".to_string(),
                    ));
                }
            };

            if request.stream {
                // 流式路径：获取完整响应后模拟流式输出
                // 将完整响应转换为模拟流式输出
                let stream = simulate_node_stream(
                    response,
                    Arc::new(ctx.clone_without_request_payloads()),
                    model.clone(),
                    request.stream_options,
                    Arc::clone(&lifecycle),
                );
                // The spawned stream task now owns client-delivery completion.
                client_response_guard.disarm();
                Ok(Sse::new(stream).into_response())
            } else {
                // 非流式路径：保持现有逻辑
                // 将 ChatCompletionResponse 转换为 OpenAI 格式
                let openai_response = ChatCompletionResponse {
                    id: format!(
                        "chatcmpl-{}-kc",
                        uuid::Uuid::new_v4()
                            .to_string()
                            .replace("-", "")
                            .to_lowercase()
                    ),
                    object: "chat.completion".to_string(),
                    created: chrono::Utc::now().timestamp(),
                    model: model.clone(),
                    choices: vec![ChatCompletionChoice {
                        index: 0,
                        message: ChatCompletionMessage {
                            role: "assistant".to_string(),
                            content: response
                                .choices
                                .first()
                                .map(|c| Value::String(c.message.content.clone())),
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        },
                        finish_reason: response
                            .choices
                            .first()
                            .and_then(|c| c.finish_reason.clone()),
                        logprobs: None,
                    }],
                    usage: CompletionUsage {
                        prompt_tokens: response.usage.prompt_tokens as u32,
                        completion_tokens: response.usage.completion_tokens as u32,
                        total_tokens: response.usage.total_tokens as u32,
                        prompt_tokens_details: None,
                        completion_tokens_details: None,
                    },
                    system_fingerprint: None,
                };

                if let Err(error) =
                    super::record_final_client_first_content(&lifecycle, ctx.request_id).await
                {
                    tracing::warn!(request_id=%ctx.request_id,%error,"failed to record Node client first content");
                }

                super::finish_client_response_trace(
                    &lifecycle,
                    &ctx,
                    ClientResponseOutcome::Succeeded,
                )
                .await;

                client_response_guard.disarm();
                Ok(Json(openai_response).into_response())
            }
        }
        ExecutionTarget::ProviderAccount {
            provider,
            account_id,
            ..
        } => {
            // Provider 执行路径：继续后续逻辑
            let (primary_provider, primary_account_id) = (provider.clone(), *account_id);

            // 5.1 根据实际 provider 更新定价（如果需要）
            {
                let ctx_mut = Arc::make_mut(&mut ctx);
                state
                    .pricing
                    .update_context_pricing(ctx_mut, &primary_provider)
                    .await;
            }

            let mut balance_reservation = match super::reserve_generation_balance(
                &state,
                &ctx,
                super::GenerationBalanceReservationLifetime::Gateway,
            )
            .await
            {
                Ok(reservation) => reservation,
                Err(error) => {
                    finish_unexecuted_trace(
                        &mut pre_execution_guard,
                        ErrorOrigin::Client,
                        TraceErrorCategory::Balance,
                        "balance_reservation_failed",
                    )
                    .await;
                    return Err(error);
                }
            };

            tracing::info!(
                request_id = %request_id.0,
                model = %request.model,
                stream = %request.stream,
                primary_provider = %primary_provider,
                "Chat completion request"
            );

            // 6. 执行（带超时保护）
            tracing::info!(
                request_id = %request_id.0,
                timeout_secs = state.gateway_config.timeout_secs,
                "Starting gateway execute"
            );

            let timeout_duration =
                std::time::Duration::from_secs(state.gateway_config.timeout_secs);
            let mut client_response_guard =
                super::ClientResponseGuard::new(Arc::clone(&lifecycle), Arc::clone(&ctx));
            pre_execution_guard.disarm();
            let rx = match tokio::time::timeout(
                timeout_duration,
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
                    balance_reservation.release().await;
                    client_response_guard
                        .finish_with_outcome(ClientResponseOutcome::ResponseFailed)
                        .await;
                    return Err(crate::error::map_openai_execution_error(
                        error,
                        ctx.client_upstream_response(),
                    ));
                }
                Err(_) => {
                    balance_reservation.release().await;
                    tracing::error!(
                        request_id = %request_id.0,
                        timeout_secs = state.gateway_config.timeout_secs,
                        "Gateway execute timeout"
                    );
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
                "Gateway execute returned, creating response"
            );

            // 7. 根据 stream 参数返回不同类型的响应
            let settlement = super::ImmediateSettlementServices::from_state(&state);
            let is_stream = request.stream;
            let model = request.model;
            let stream_options = request.stream_options;

            if is_stream {
                // Pure-text streams wait for their first protocol event so a
                // pre-stream rejection can retain its native HTTP status. The
                // multimodal keepalive path instead commits once upstream HTTP
                // acceptance is known, allowing keepalives during slow image
                // processing without hiding an actual HTTP rejection.
                // 流式响应
                if has_image_content(&ctx.messages) {
                    // 流式 + 多模态：SSE keepalive 防止图片下载超时
                    let error_ctx = Arc::clone(&ctx);
                    let (initial_status_tx, initial_status_rx) = tokio::sync::oneshot::channel();
                    let stream = create_openai_stream_with_keepalive_and_lifecycle(
                        rx,
                        OpenAiStreamContext {
                            ctx,
                            model,
                            provider_name: primary_provider,
                            account_id: primary_account_id,
                            settlement,
                            stream_options,
                            lifecycle: Arc::clone(&lifecycle),
                            initial_event: None,
                            initial_status: Some(initial_status_tx),
                            body_permit: body_permit.map(|Extension(permit)| permit),
                        },
                        timeout_duration,
                    );
                    balance_reservation.transfer_to_settlement();
                    client_response_guard.disarm();
                    if !matches!(
                        super::await_initial_stream_status(&error_ctx, initial_status_rx).await,
                        super::InitialStreamStatus::Ready
                    ) {
                        drop(stream);
                        return Err(error_ctx
                            .client_upstream_response()
                            .map(ApiError::OpenAiUpstream)
                            .unwrap_or_else(|| {
                                ApiError::Provider("Upstream request failed".to_string())
                            }));
                    }
                    Ok(Sse::new(stream).into_response())
                } else {
                    // 流式 + 纯文本：原逻辑，无 keepalive
                    let error_ctx = Arc::clone(&ctx);
                    let (initial_status_tx, initial_status_rx) = tokio::sync::oneshot::channel();
                    let stream = create_openai_stream_with_lifecycle(
                        rx,
                        OpenAiStreamContext {
                            ctx,
                            model,
                            provider_name: primary_provider,
                            account_id: primary_account_id,
                            settlement,
                            stream_options,
                            lifecycle: Arc::clone(&lifecycle),
                            initial_event: None,
                            initial_status: Some(initial_status_tx),
                            body_permit: body_permit.map(|Extension(permit)| permit),
                        },
                    );
                    balance_reservation.transfer_to_settlement();
                    client_response_guard.disarm();
                    if !matches!(
                        initial_status_rx.await,
                        Ok(super::InitialStreamStatus::Ready)
                    ) {
                        drop(stream);
                        return Err(error_ctx
                            .client_upstream_response()
                            .map(ApiError::OpenAiUpstream)
                            .unwrap_or_else(|| {
                                ApiError::Provider("Upstream request failed".to_string())
                            }));
                    }
                    Ok(Sse::new(stream).into_response())
                }
            } else {
                // A non-streaming JSON response must retain its real HTTP error
                // status. Whitespace keepalives commit 200 before the upstream
                // result is known and break SDK retry/error handling. Clients
                // that need incremental liveness should request SSE instead.
                client_response_guard.disarm();
                balance_reservation.transfer_to_settlement();
                let response = create_openai_response_with_lifecycle(
                    rx,
                    OpenAiJsonRuntime {
                        ctx,
                        model,
                        provider_name: primary_provider,
                        account_id: primary_account_id,
                        settlement,
                        lifecycle: Arc::clone(&lifecycle),
                        body_permit: body_permit.map(|Extension(permit)| permit),
                    },
                )
                .await?;
                openai_json_response(response)
            }
        }
    }
}

/// Selecting a Node route does not mean a task has been queued yet. The Node
/// gateway advances the trace to `queued` only after its PostgreSQL task row is
/// created, which also keeps the queued-task metric aligned with real tasks.
fn initial_route_trace_state(target: &ExecutionTarget) -> (RouteType, RequestStatus) {
    match target {
        ExecutionTarget::Node { .. } => (RouteType::Node, RequestStatus::Routing),
        ExecutionTarget::ProviderAccount { .. } => {
            (RouteType::ProviderAccount, RequestStatus::Routing)
        }
    }
}

struct OpenAiJsonRuntime {
    ctx: Arc<RequestContext>,
    model: String,
    provider_name: String,
    account_id: uuid::Uuid,
    settlement: super::ImmediateSettlementServices,
    lifecycle: Arc<dyn keycompute_types::RequestLifecycleRecorder>,
    body_permit: Option<crate::state::GenerationHttpBodyPermit>,
}

async fn create_openai_response_with_lifecycle(
    mut rx: tokio::sync::mpsc::Receiver<llm_protocol_provider::StreamEvent>,
    runtime: OpenAiJsonRuntime,
) -> Result<OpenAiJsonResponse> {
    let OpenAiJsonRuntime {
        ctx,
        model,
        provider_name,
        account_id,
        settlement,
        lifecycle,
        body_permit,
    } = runtime;
    let mut client_response_guard =
        super::ClientResponseGuard::new(Arc::clone(&lifecycle), Arc::clone(&ctx));
    let (mut response_tx, response_rx) = tokio::sync::oneshot::channel();
    let worker_ctx = Arc::clone(&ctx);
    tokio::spawn(async move {
        let _body_permit = body_permit;
        let completion_id = generate_completion_id();
        let created = chrono::Utc::now().timestamp();
        let mut collector = StreamCollector::new();
        let mut handler_connected = true;
        let mut terminal_error = None;

        // The worker owns the upstream receiver and billing state. If Axum
        // drops the handler before a response exists, cancel upstream work but
        // keep draining until executor supplies its terminal event.
        loop {
            tokio::select! {
                biased;
                _ = response_tx.closed(), if handler_connected => {
                    handler_connected = false;
                    worker_ctx.mark_client_disconnected();
                }
                event = rx.recv() => {
                    let Some(event) = event else { break };
                    match collector.process_event(event) {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(message) => {
                            tracing::error!(
                                request_id = %worker_ctx.request_id,
                                error = %message,
                                "Stream error during non-streaming response"
                            );
                            terminal_error = Some(
                                worker_ctx
                                    .client_upstream_response()
                                    .map(ApiError::OpenAiUpstream)
                                    .unwrap_or_else(|| ApiError::Provider(message)),
                            );
                            break;
                        }
                    }
                }
            }
        }

        if terminal_error.is_none() {
            collector.check_completion(&worker_ctx.request_id);
            if collector.status == "incomplete" {
                terminal_error = Some(ApiError::Internal(
                    "Stream ended unexpectedly: channel closed without Done/Error event"
                        .to_string(),
                ));
            }
        }

        finalize_openai_billing(
            &settlement,
            &worker_ctx,
            &provider_name,
            account_id,
            &collector.status,
        )
        .await;

        let result = if let Some(error) = terminal_error {
            Err(error)
        } else {
            let (prompt_tokens, completion_tokens) = worker_ctx.usage_snapshot();
            if let Some(response) = collector.native_chat_response.take() {
                Ok(response)
            } else {
                serde_json::to_value(build_chat_completion_response(
                    completion_id,
                    created,
                    model,
                    collector.content,
                    collector.finish_reason,
                    prompt_tokens,
                    completion_tokens,
                    provider_name,
                ))
                .map(|body| OpenAiJsonResponse {
                    body,
                    admission: None,
                })
                .map_err(|error| {
                    ApiError::Internal(format!(
                        "Failed to serialize Chat Completions response: {error}"
                    ))
                })
            }
        };
        if handler_connected && response_tx.send(result).is_err() {
            worker_ctx.mark_client_disconnected();
        }
    });

    let response = match response_rx.await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            super::finish_client_response_trace(
                &lifecycle,
                &ctx,
                ClientResponseOutcome::ResponseFailed,
            )
            .await;
            client_response_guard.disarm();
            return Err(error);
        }
        Err(_) => {
            super::finish_client_response_trace(
                &lifecycle,
                &ctx,
                ClientResponseOutcome::ResponseFailed,
            )
            .await;
            client_response_guard.disarm();
            return Err(ApiError::Internal(
                "Non-streaming response worker stopped unexpectedly".to_string(),
            ));
        }
    };
    if let Err(error) = super::record_final_client_first_content(&lifecycle, ctx.request_id).await {
        tracing::warn!(request_id = %ctx.request_id, %error, "failed to record client first content");
    }
    super::finish_client_response_trace(&lifecycle, &ctx, ClientResponseOutcome::Succeeded).await;
    client_response_guard.disarm();
    Ok(response)
}

#[derive(Debug)]
struct OpenAiJsonResponse {
    body: Value,
    admission: Option<LargeBodyPermit>,
}

struct GuardedOpenAiBytes<G> {
    bytes: bytes::Bytes,
    _guard: G,
}

impl<G> AsRef<[u8]> for GuardedOpenAiBytes<G> {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

fn retain_openai_bytes_guard<G>(bytes: bytes::Bytes, guard: G) -> bytes::Bytes
where
    G: Send + Sync + 'static,
{
    bytes::Bytes::from_owner(GuardedOpenAiBytes {
        bytes,
        _guard: guard,
    })
}

fn serialize_openai_json_body(
    response: OpenAiJsonResponse,
) -> std::result::Result<bytes::Bytes, serde_json::Error> {
    let bytes = bytes::Bytes::from(serde_json::to_vec(&response.body)?);
    Ok(match response.admission {
        Some(admission) => retain_openai_bytes_guard(bytes, admission),
        None => bytes,
    })
}

fn openai_json_response(response: OpenAiJsonResponse) -> Result<axum::response::Response> {
    let body = serialize_openai_json_body(response).map_err(|error| {
        ApiError::Internal(format!(
            "Failed to serialize Chat Completions response: {error}"
        ))
    })?;
    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .map_err(|error| {
            ApiError::Internal(format!(
                "Failed to build Chat Completions response: {error}"
            ))
        })
}

/// 检测消息列表中是否包含需要网络下载的图片 URL
///
/// 仅当存在 `ContentPart::ImageUrl` 且 URL 为 HTTP(S) 协议（非 data URI）时才返回 true。
/// data URI（如 `data:image/png;base64,...`）图片数据已内嵌在请求体中，
/// 上游 Provider 无需额外网络下载即可处理，不会触发超时问题。
/// 纯文本或仅有文本块的 Parts 不属于多模态。
fn has_image_content(messages: &[Message]) -> bool {
    messages.iter().any(|m| match &m.content {
        MessageContent::Parts(parts) => parts.iter().any(|p| match p {
            ContentPart::ImageUrl { image_url } => !image_url.url.starts_with("data:"),
            _ => false,
        }),
        MessageContent::Text(_) => false,
    })
}

/// 构建 OpenAI 格式的 ChatCompletion 响应。
#[allow(clippy::too_many_arguments)]
fn build_chat_completion_response(
    completion_id: String,
    created: i64,
    model: String,
    content: String,
    finish_reason: Option<String>,
    prompt_tokens: u32,
    completion_tokens: u32,
    provider_name: String,
) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: completion_id,
        object: "chat.completion".to_string(),
        created,
        model,
        choices: vec![ChatCompletionChoice {
            index: 0,
            message: ChatCompletionMessage {
                role: "assistant".to_string(),
                content: Some(Value::String(content)),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            finish_reason,
            logprobs: None,
        }],
        usage: CompletionUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        },
        system_fingerprint: Some(format!("fp_{}", provider_name)),
    }
}

/// 流事件收集器
///
/// 封装非流式响应路径的事件处理状态与逻辑。
struct StreamCollector {
    content: String,
    finish_reason: Option<String>,
    native_chat_response: Option<OpenAiJsonResponse>,
    status: String,
    completed: bool,
}

impl StreamCollector {
    fn new() -> Self {
        Self {
            content: String::new(),
            finish_reason: None,
            native_chat_response: None,
            status: "success".to_string(),
            completed: false,
        }
    }

    /// 处理单个流事件
    ///
    /// 返回值：
    /// - `Ok(true)` — 继续收集
    /// - `Ok(false)` — 流正常结束（收到 Done 事件）
    /// - `Err(message)` — 流异常（收到 Error 事件），调用者负责执行计费并决定错误输出方式
    fn process_event(
        &mut self,
        event: llm_protocol_provider::StreamEvent,
    ) -> std::result::Result<bool, String> {
        match event {
            llm_protocol_provider::StreamEvent::Delta {
                content: delta,
                finish_reason: reason,
            } => {
                self.content.push_str(&delta);
                if reason.is_some() {
                    self.finish_reason = reason;
                }
                Ok(true)
            }
            llm_protocol_provider::StreamEvent::Done => {
                self.completed = true;
                Ok(false)
            }
            llm_protocol_provider::StreamEvent::Error { message } => {
                self.status = "error".to_string();
                Err(message)
            }
            llm_protocol_provider::StreamEvent::Native {
                event: NativeStreamEvent::OpenAiChatJson { body, admission },
            } => {
                self.native_chat_response = Some(OpenAiJsonResponse { body, admission });
                Ok(true)
            }
            llm_protocol_provider::StreamEvent::Usage { .. }
            | llm_protocol_provider::StreamEvent::InputUsage { .. }
            | llm_protocol_provider::StreamEvent::Raw { .. }
            | llm_protocol_provider::StreamEvent::Native { .. } => Ok(true),
        }
    }

    /// 检查流是否意外结束（channel 关闭但没有收到 Done/Error 事件）
    fn check_completion(&mut self, request_id: &uuid::Uuid) {
        if !self.completed {
            tracing::warn!(
                request_id = %request_id,
                "Non-streaming response: channel closed without Done/Error event"
            );
            self.status = "incomplete".to_string();
        }
    }
}

/// Finalize an OpenAI-compatible request without attributing a successful
/// fallback to the primary provider account.
async fn finalize_openai_billing(
    settlement: &super::ImmediateSettlementServices,
    ctx: &RequestContext,
    primary_provider: &str,
    primary_account_id: uuid::Uuid,
    status: &str,
) {
    super::finalize_immediate_settlement_logged(
        settlement,
        ctx,
        primary_provider,
        primary_account_id,
        status,
        "openai",
    )
    .await;
}

/// 生成 OpenAI 格式的 completion ID
fn generate_completion_id() -> String {
    format!(
        "chatcmpl-{}-kc",
        uuid::Uuid::new_v4()
            .to_string()
            .replace("-", "")
            .to_lowercase()
    )
}

/// 构建流式 Delta chunk 的 SSE 数据字符串
///
/// 供 `create_openai_stream` 与 `create_openai_stream_with_keepalive` 共享，
/// 消除 chunk 构建逻辑的重复。仅首个 chunk 携带 `role: "assistant"`，
/// 遵循 OpenAI SSE 协议规范。
fn make_delta_chunk_data(
    content: String,
    finish_reason: &Option<String>,
    first_chunk: &mut bool,
    completion_id: &str,
    created: i64,
    model: &str,
    provider_name: &str,
) -> String {
    let delta = if *first_chunk {
        *first_chunk = false;
        ChatCompletionChunkDelta {
            role: Some("assistant".to_string()),
            content: Some(content),
            tool_calls: None,
        }
    } else {
        ChatCompletionChunkDelta {
            role: None,
            content: Some(content),
            tool_calls: None,
        }
    };

    let chunk = ChatCompletionChunk {
        id: completion_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        system_fingerprint: Some(format!("fp_{}", provider_name)),
        choices: vec![ChatCompletionChunkChoice {
            index: 0,
            delta,
            finish_reason: finish_reason.clone(),
            logprobs: None,
        }],
        usage: None,
    };

    serde_json::to_string(&chunk).unwrap_or_else(|e| {
        tracing::error!(
            completion_id = %completion_id,
            error = %e,
            "Failed to serialize delta chunk"
        );
        serde_json::json!({
            "error": {
                "message": "Internal error: failed to serialize delta chunk",
                "type": "server_error",
                "param": null,
                "code": null
            }
        })
        .to_string()
    })
}

/// 构建流式 Usage chunk 的 SSE 数据字符串
///
/// 供 `create_openai_stream` 与 `create_openai_stream_with_keepalive` 共享。
fn make_usage_chunk_data(
    input_tokens: u32,
    output_tokens: u32,
    completion_id: &str,
    created: i64,
    model: &str,
    provider_name: &str,
) -> String {
    let usage_chunk = ChatCompletionChunk {
        id: completion_id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        system_fingerprint: Some(format!("fp_{}", provider_name)),
        choices: vec![],
        usage: Some(CompletionUsage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
            total_tokens: input_tokens + output_tokens,
            prompt_tokens_details: None,
            completion_tokens_details: None,
        }),
    };

    serde_json::to_string(&usage_chunk).unwrap_or_else(|e| {
        tracing::error!(
            completion_id = %completion_id,
            error = %e,
            "Failed to serialize usage chunk"
        );
        openai_error_chunk(
            "Internal error: failed to serialize usage chunk",
            "server_error",
            None,
        )
    })
}

/// OpenAI 错误帧 JSON：对客户端只暴露通用文本，不泄露上游细节。
///
/// SSE 与 chunked 非流式路径共用同一错误形状；`code` 为 `None` 时输出
/// `null`（OpenAI API 对部分错误不提供机器码）。
fn openai_error_chunk(message: &str, error_type: &str, code: Option<&str>) -> String {
    serde_json::json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": null,
            "code": code
        }
    })
    .to_string()
}

fn native_chat_chunk_has_content(data: &Value) -> bool {
    data.get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                let Some(delta) = choice.get("delta") else {
                    return false;
                };
                delta
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| !content.is_empty())
                    || delta
                        .get("refusal")
                        .and_then(Value::as_str)
                        .is_some_and(|refusal| !refusal.is_empty())
                    || delta
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .is_some_and(|calls| !calls.is_empty())
            })
        })
}

fn native_chat_chunk_has_usage(data: &Value) -> bool {
    data.pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
        .is_some()
        && data
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .is_some()
}

/// Bound SSE backpressure so a client that stops reading cannot indefinitely
/// block the worker that owns upstream draining and billing settlement.
const OPENAI_SSE_SEND_TIMEOUT: Duration = Duration::from_secs(30);

struct OpenAiStreamContext {
    ctx: Arc<RequestContext>,
    model: String,
    provider_name: String,
    account_id: uuid::Uuid,
    settlement: super::ImmediateSettlementServices,
    stream_options: Option<StreamOptions>,
    lifecycle: Arc<dyn keycompute_types::RequestLifecycleRecorder>,
    initial_event: Option<llm_protocol_provider::StreamEvent>,
    initial_status: Option<tokio::sync::oneshot::Sender<super::InitialStreamStatus>>,
    body_permit: Option<crate::state::GenerationHttpBodyPermit>,
}

fn openai_sse_channel_capacity(ctx: &RequestContext) -> usize {
    if ctx.native_openai_chat_request.is_some() {
        LARGE_NATIVE_EVENT_CHANNEL_CAPACITY
    } else {
        100
    }
}

async fn forward_openai_sse_event(
    sse_tx: &mpsc::Sender<Event>,
    ctx: &RequestContext,
    client_connected: &mut bool,
    event: Event,
) -> bool {
    if !*client_connected {
        return false;
    }
    let sent = tokio::time::timeout(OPENAI_SSE_SEND_TIMEOUT, sse_tx.send(event))
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false);
    if !sent {
        *client_connected = false;
        ctx.mark_client_disconnected();
    }
    sent
}

/// 创建 OpenAI 格式的 SSE 流
#[cfg(test)]
fn create_openai_stream(
    rx: tokio::sync::mpsc::Receiver<llm_protocol_provider::StreamEvent>,
    ctx: Arc<RequestContext>,
    model: String,
    provider_name: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
    stream_options: Option<StreamOptions>,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    create_openai_stream_with_lifecycle(
        rx,
        OpenAiStreamContext {
            ctx,
            model,
            provider_name,
            account_id,
            settlement: super::ImmediateSettlementServices::for_test(billing),
            stream_options,
            lifecycle: Arc::new(keycompute_types::NoopRequestLifecycleRecorder),
            initial_event: None,
            initial_status: None,
            body_permit: None,
        },
    )
}

fn create_openai_stream_with_lifecycle(
    mut rx: tokio::sync::mpsc::Receiver<llm_protocol_provider::StreamEvent>,
    stream_context: OpenAiStreamContext,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    let (sse_tx, sse_rx) = mpsc::channel(openai_sse_channel_capacity(&stream_context.ctx));
    let OpenAiStreamContext {
        ctx,
        model,
        provider_name,
        account_id,
        settlement,
        stream_options,
        lifecycle,
        mut initial_event,
        mut initial_status,
        body_permit,
    } = stream_context;

    // The worker, rather than the HTTP body, owns the upstream receiver and
    // billing context. Dropping the response therefore cannot skip settlement.
    tokio::spawn(async move {
        let _body_permit = body_permit;
        let mut status = "success";
        let mut completed = false;
        let mut first_chunk = true;
        let mut client_connected = true;
        let mut client_first_content_recorded = false;
        let mut native_usage_forwarded = false;
        let completion_id = generate_completion_id();
        let created = chrono::Utc::now().timestamp();

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
                    super::report_initial_stream_status(&mut initial_status, event.as_ref());
                    let Some(event) = event else { break };
                    match event {
                        llm_protocol_provider::StreamEvent::Delta { content, finish_reason } => {
                            let has_content = !content.is_empty();
                            let data = make_delta_chunk_data(
                                content, &finish_reason, &mut first_chunk,
                                &completion_id, created, &model, &provider_name,
                            );
                            let sent = forward_openai_sse_event(
                                &sse_tx,
                                &ctx,
                                &mut client_connected,
                                Event::default().data(data),
                            )
                            .await;
                            if sent && has_content && !client_first_content_recorded {
                                if let Err(error) = lifecycle
                                    .record_client_first_content(ctx.request_id, chrono::Utc::now())
                                    .await
                                {
                                    tracing::warn!(request_id = %ctx.request_id, %error, "failed to record client first content");
                                }
                                client_first_content_recorded = true;
                            }
                        }
                        llm_protocol_provider::StreamEvent::Done => {
                            completed = true;
                            finalize_openai_billing(
                                &settlement,
                                &ctx,
                                &provider_name,
                                account_id,
                                status,
                            )
                            .await;
                            if stream_options.as_ref().is_some_and(|o| o.include_usage)
                                && !native_usage_forwarded
                            {
                                let (input_tokens, output_tokens) = ctx.usage_snapshot();
                                let data = make_usage_chunk_data(
                                    input_tokens, output_tokens,
                                    &completion_id, created, &model, &provider_name,
                                );
                                let _ = forward_openai_sse_event(
                                    &sse_tx, &ctx, &mut client_connected,
                                    Event::default().data(data),
                                )
                                .await;
                            }
                            let _ = forward_openai_sse_event(
                                &sse_tx, &ctx, &mut client_connected,
                                Event::default().data("[DONE]"),
                            )
                            .await;
                            super::finish_client_response_trace(
                                &lifecycle,
                                &ctx,
                                ClientResponseOutcome::Succeeded,
                            )
                            .await;
                            break;
                        }
                        llm_protocol_provider::StreamEvent::Error { message } => {
                            completed = true;
                            status = "error";
                            finalize_openai_billing(
                                &settlement,
                                &ctx,
                                &provider_name,
                                account_id,
                                status,
                            )
                            .await;
                            tracing::warn!(
                                request_id = %ctx.request_id,
                                error = %message,
                                "OpenAI upstream stream failed"
                            );
                            let _ = forward_openai_sse_event(
                                &sse_tx, &ctx, &mut client_connected,
                                Event::default().data(openai_error_chunk(
                                    "Upstream request failed", "api_error", Some("internal_error"),
                                )),
                            )
                            .await;
                            let _ = forward_openai_sse_event(
                                &sse_tx, &ctx, &mut client_connected,
                                Event::default().data("[DONE]"),
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
                        llm_protocol_provider::StreamEvent::Native {
                            event: NativeStreamEvent::OpenAiChatSse { data },
                        } => {
                            let has_content = native_chat_chunk_has_content(&data);
                            native_usage_forwarded |= native_chat_chunk_has_usage(&data);
                            let sent = forward_openai_sse_event(
                                &sse_tx,
                                &ctx,
                                &mut client_connected,
                                Event::default().data(data.to_string()),
                            )
                            .await;
                            if sent && has_content && !client_first_content_recorded {
                                if let Err(error) = lifecycle
                                    .record_client_first_content(ctx.request_id, chrono::Utc::now())
                                    .await
                                {
                                    tracing::warn!(request_id = %ctx.request_id, %error, "failed to record client first content");
                                }
                                client_first_content_recorded = true;
                            }
                        }
                        llm_protocol_provider::StreamEvent::Usage { .. }
                        | llm_protocol_provider::StreamEvent::InputUsage { .. }
                        | llm_protocol_provider::StreamEvent::Raw { .. }
                        | llm_protocol_provider::StreamEvent::Native { .. } => {}
                    }
                }
            }
        }

        if !completed {
            tracing::warn!(
                request_id = %ctx.request_id,
                "Stream ended without Done or Error event"
            );
            status = "incomplete";
            finalize_openai_billing(&settlement, &ctx, &provider_name, account_id, status).await;
            let _ = forward_openai_sse_event(
                &sse_tx,
                &ctx,
                &mut client_connected,
                Event::default().data(openai_error_chunk(
                    "Stream ended unexpectedly",
                    "api_error",
                    Some("internal_error"),
                )),
            )
            .await;
            let _ = forward_openai_sse_event(
                &sse_tx,
                &ctx,
                &mut client_connected,
                Event::default().data("[DONE]"),
            )
            .await;
            super::finish_client_response_trace(
                &lifecycle,
                &ctx,
                ClientResponseOutcome::ResponseFailed,
            )
            .await;
        }
    });

    ReceiverStream::new(sse_rx).map(Ok)
}

/// 创建带 keepalive 的 SSE 流式响应（多模态专用）。
///
/// 图片下载期间每 10s 发送 SSE 空事件，防止 Nginx / 云平台
/// `proxy_read_timeout` 超时触发 504。
///
/// SSE 空事件（`data:\\n\\n`）对 OpenAI 兼容客户端透明，
/// 客户端 parser 会忽略空 data 字段。
fn create_openai_stream_with_keepalive_and_lifecycle(
    mut rx: tokio::sync::mpsc::Receiver<llm_protocol_provider::StreamEvent>,
    stream_context: OpenAiStreamContext,
    response_timeout: Duration,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    let (sse_tx, sse_rx) = mpsc::channel(openai_sse_channel_capacity(&stream_context.ctx));
    let OpenAiStreamContext {
        ctx,
        model,
        provider_name,
        account_id,
        settlement,
        stream_options,
        lifecycle,
        mut initial_event,
        mut initial_status,
        body_permit,
    } = stream_context;

    tokio::spawn(async move {
        let _body_permit = body_permit;
        let mut status = "success";
        let mut first_chunk = true;
        let mut client_connected = true;
        let mut client_first_content_recorded = false;
        let mut native_usage_forwarded = false;
        let completion_id = generate_completion_id();
        let created = chrono::Utc::now().timestamp();

        // 响应期限与 executor 使用同一 Gateway timeout 配置，既防止上游
        // 后台任务停滞导致无限 keepalive，也不会截断运维明确放宽的超时。
        let deadline = tokio::time::sleep(response_timeout);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = sse_tx.closed(), if client_connected => {
                    client_connected = false;
                    ctx.mark_client_disconnected();
                }
                _ = &mut deadline => {
                    super::report_initial_stream_status(&mut initial_status, None);
                    status = "timeout";
                    tracing::error!(
                        request_id = %ctx.request_id,
                        timeout_secs = response_timeout.as_secs(),
                        "SSE stream keepalive response deadline exceeded"
                    );
                    finalize_openai_billing(
                        &settlement,
                        &ctx,
                        &provider_name,
                        account_id,
                        status,
                    )
                    .await;
                    let _ = forward_openai_sse_event(
                        &sse_tx, &ctx, &mut client_connected,
                        Event::default().data(openai_error_chunk(
                            "Request timed out", "server_error", Some("timeout"),
                        )),
                    )
                    .await;
                    let _ = forward_openai_sse_event(
                        &sse_tx, &ctx, &mut client_connected,
                        Event::default().data("[DONE]"),
                    )
                    .await;
                    super::finish_client_response_trace(
                        &lifecycle,
                        &ctx,
                        ClientResponseOutcome::TimedOut,
                    )
                    .await;
                    return;
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)), if client_connected => {
                    let _ = forward_openai_sse_event(
                        &sse_tx, &ctx, &mut client_connected, Event::default().data(""),
                    )
                    .await;
                }
                event = async {
                    if initial_event.is_some() {
                        initial_event.take()
                    } else {
                        rx.recv().await
                    }
                } => {
                    super::report_initial_stream_status(&mut initial_status, event.as_ref());
                    match event {
                        Some(event) => match event {
                            llm_protocol_provider::StreamEvent::Delta { content, finish_reason } => {
                                let has_content = !content.is_empty();
                                let data = make_delta_chunk_data(
                                    content, &finish_reason, &mut first_chunk,
                                    &completion_id, created, &model, &provider_name,
                                );
                                let sent = forward_openai_sse_event(
                                    &sse_tx, &ctx, &mut client_connected,
                                    Event::default().data(data),
                                )
                                .await;
                                if sent && has_content && !client_first_content_recorded {
                                    if let Err(error) = lifecycle
                                        .record_client_first_content(ctx.request_id, chrono::Utc::now())
                                        .await
                                    {
                                        tracing::warn!(request_id = %ctx.request_id, %error, "failed to record client first content");
                                    }
                                    client_first_content_recorded = true;
                                }
                            }
                            llm_protocol_provider::StreamEvent::Done => {
                                finalize_openai_billing(
                                    &settlement,
                                    &ctx,
                                    &provider_name,
                                    account_id,
                                    status,
                                )
                                .await;

                                if stream_options.as_ref().is_some_and(|o| o.include_usage)
                                    && !native_usage_forwarded
                                {
                                    let (input_tokens, output_tokens) = ctx.usage_snapshot();
                                    let data = make_usage_chunk_data(
                                        input_tokens, output_tokens,
                                        &completion_id, created, &model, &provider_name,
                                    );
                                    let _ = forward_openai_sse_event(
                                        &sse_tx, &ctx, &mut client_connected,
                                        Event::default().data(data),
                                    )
                                    .await;
                                }

                                let _ = forward_openai_sse_event(
                                    &sse_tx, &ctx, &mut client_connected,
                                    Event::default().data("[DONE]"),
                                )
                                .await;
                                super::finish_client_response_trace(
                                    &lifecycle,
                                    &ctx,
                                    ClientResponseOutcome::Succeeded,
                                )
                                .await;
                                return;
                            }
                            llm_protocol_provider::StreamEvent::Error { message } => {
                                status = "error";
                                finalize_openai_billing(
                                    &settlement,
                                    &ctx,
                                    &provider_name,
                                    account_id,
                                    status,
                                )
                                .await;
                                // 不向客户端暴露上游错误细节：原始消息只记录日志。
                                tracing::warn!(
                                    request_id = %ctx.request_id,
                                    error = %message,
                                    "OpenAI upstream stream failed"
                                );
                                let _ = forward_openai_sse_event(
                                    &sse_tx, &ctx, &mut client_connected,
                                    Event::default().data(openai_error_chunk(
                                        "Upstream request failed", "api_error", Some("internal_error"),
                                    )),
                                )
                                .await;
                                let _ = forward_openai_sse_event(
                                    &sse_tx, &ctx, &mut client_connected,
                                    Event::default().data("[DONE]"),
                                )
                                .await;
                                super::finish_client_response_trace(
                                    &lifecycle,
                                    &ctx,
                                    ClientResponseOutcome::ResponseFailed,
                                )
                                .await;
                                return;
                            }
                            llm_protocol_provider::StreamEvent::Native {
                                event: NativeStreamEvent::OpenAiChatSse { data },
                            } => {
                                let has_content = native_chat_chunk_has_content(&data);
                                native_usage_forwarded |= native_chat_chunk_has_usage(&data);
                                let sent = forward_openai_sse_event(
                                    &sse_tx,
                                    &ctx,
                                    &mut client_connected,
                                    Event::default().data(data.to_string()),
                                )
                                .await;
                                if sent && has_content && !client_first_content_recorded {
                                    if let Err(error) = lifecycle
                                        .record_client_first_content(
                                            ctx.request_id,
                                            chrono::Utc::now(),
                                        )
                                        .await
                                    {
                                        tracing::warn!(request_id = %ctx.request_id, %error, "failed to record client first content");
                                    }
                                    client_first_content_recorded = true;
                                }
                            }
                            llm_protocol_provider::StreamEvent::Usage { .. }
                            | llm_protocol_provider::StreamEvent::InputUsage { .. }
                            | llm_protocol_provider::StreamEvent::Raw { .. }
                            | llm_protocol_provider::StreamEvent::Native { .. } => {
                                // Usage 由 executor 层通过 ctx.set_*_tokens() 消费，
                                // Raw 为 provider 原始事件不需要透传
                            }
                        },
                        None => break,
                    }
                }
            }
        }

        // 流意外结束（channel 关闭但没有收到完成事件）
        // 所有正常完成路径（Done / Error / deadline）均使用 return 退出，
        // 只有 channel 关闭（None）通过 break 到达此处
        tracing::warn!(
            request_id = %ctx.request_id,
            "SSE stream keepalive: ended without Done or Error event"
        );
        status = "incomplete";
        finalize_openai_billing(&settlement, &ctx, &provider_name, account_id, status).await;
        let _ = forward_openai_sse_event(
            &sse_tx,
            &ctx,
            &mut client_connected,
            Event::default().data(openai_error_chunk(
                "Stream ended unexpectedly",
                "api_error",
                Some("internal_error"),
            )),
        )
        .await;
        let _ = forward_openai_sse_event(
            &sse_tx,
            &ctx,
            &mut client_connected,
            Event::default().data("[DONE]"),
        )
        .await;
        super::finish_client_response_trace(
            &lifecycle,
            &ctx,
            ClientResponseOutcome::ResponseFailed,
        )
        .await;
    });

    ReceiverStream::new(sse_rx).map(Ok)
}

// ==================== Models ====================

/// 模型信息
#[derive(Debug, Serialize, Deserialize)]
pub struct Model {
    /// 模型 ID
    pub id: String,
    /// 对象类型: model
    pub object: String,
    /// 创建时间戳
    pub created: i64,
    /// 拥有者
    pub owned_by: String,
}

/// 模型列表响应
#[derive(Debug, Serialize, Deserialize)]
pub struct ListModelsResponse {
    /// 对象类型: list
    pub object: String,
    /// 模型列表
    pub data: Vec<Model>,
}

/// 模型列表查询参数
#[derive(Debug, Deserialize)]
pub struct ListModelsQuery {
    /// 入口协议（openai / anthropic），缺省 openai。
    ///
    /// 与路由的入口协议隔离保持一致：/v1/models 是 OpenAI 兼容入口，
    /// 缺省只列出 openai 协议账号声明的模型，避免列出当前端点无法
    /// 服务的模型（否则列表中的 Claude 模型在 /v1/chat/completions
    /// 会得到 404）；`protocol=anthropic` 供需要 Anthropic 模型清单的
    /// 消费方使用（如 web 端 Anthropic 示例）。
    #[serde(default)]
    pub protocol: Option<String>,
    /// Optional API surface capability (`chat_completions`, `responses`, or
    /// `messages`). This keeps example pickers from advertising a model whose
    /// accounts cannot serve the selected endpoint.
    #[serde(default)]
    pub capability: Option<String>,
}

/// 按入口协议收集模型清单：仅保留指定协议账号声明的模型。
///
/// 提取为纯函数便于单元测试（handler 级测试需构造完整 AppState，
/// 成本高且不必要）。
fn collect_models_by_protocol(
    accounts: impl IntoIterator<Item = Account>,
    protocol: &str,
    capability: Option<AccountApiCapability>,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashMap<String, String>,
) {
    let mut model_set = std::collections::HashSet::new();
    let mut provider_map = std::collections::HashMap::new();
    for account in accounts.into_iter().filter(|account| {
        account.provider == protocol
            && capability.is_none_or(|capability| {
                account
                    .api_capabilities
                    .iter()
                    .any(|value| value == capability.as_str())
            })
    }) {
        for model in account.models_supported {
            model_set.insert(model.clone());
            provider_map.insert(model, account.provider.clone());
        }
    }
    (model_set, provider_map)
}

fn resolve_list_capability(
    protocol: &str,
    capability: Option<&str>,
) -> Result<Option<AccountApiCapability>> {
    let Some(value) = capability else {
        return Ok(None);
    };
    let capability = AccountApiCapability::parse(value).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "Unsupported capability '{value}', expected one of: chat_completions, responses, messages"
        ))
    })?;
    let compatible = matches!(
        (protocol, capability),
        (
            "openai",
            AccountApiCapability::ChatCompletions | AccountApiCapability::Responses
        ) | ("anthropic", AccountApiCapability::Messages)
    );
    if !compatible {
        return Err(ApiError::BadRequest(format!(
            "Capability '{}' is not valid for protocol '{protocol}'",
            capability.as_str()
        )));
    }
    Ok(Some(capability))
}

/// 解析模型列表的入口协议参数：规范化大小写并校验合法性。
///
/// 提取为纯函数便于单元测试（handler 级测试需构造完整 AppState，
/// 成本高且不必要）。
fn resolve_list_protocol(protocol: Option<&str>) -> Result<&'static str> {
    match protocol {
        Some(p) => match llm_protocol_provider::ProtocolType::parse(p) {
            Some(pt) => Ok(pt.as_str()),
            None => Err(ApiError::BadRequest(format!(
                "Unsupported protocol '{p}', expected one of: openai, anthropic"
            ))),
        },
        // 缺省按 openai 入口过滤（与 /v1/chat/completions 的隔离一致）
        None => Ok("openai"),
    }
}

/// 列出所有模型
/// GET /v1/models
/// 从数据库聚合指定入口协议（缺省 openai）的启用账号支持的模型列表
pub async fn list_models(
    State(state): State<AppState>,
    Query(query): Query<ListModelsQuery>,
) -> Result<Json<ListModelsResponse>> {
    let protocol = resolve_list_protocol(query.protocol.as_deref())?;
    let capability = resolve_list_capability(protocol, query.capability.as_deref())?;

    let (mut model_set, mut provider_map) = (
        std::collections::HashSet::new(),
        std::collections::HashMap::new(),
    );

    // 尝试从数据库获取模型列表
    if let Some(pool) = state.pool.as_deref() {
        // 查询所有启用的账号（不限制 tenant_id，使用系统级查询）
        if let Ok(accounts) = Account::find_enabled_all(pool).await {
            (model_set, provider_map) = collect_models_by_protocol(accounts, protocol, capability);
        }
    }

    // 如果数据库中没有模型，使用默认模型列表（仅保留一个示例模型）
    if model_set.is_empty() {
        model_set.insert("model-empty".to_string());

        // 使用 provideraccount 计费维度
        let provider = keycompute_pricing::DEFAULT_PRICING_PROVIDER;
        provider_map.insert("model-empty".to_string(), provider.to_string());
    }

    let models: Vec<Model> = model_set
        .into_iter()
        .map(|id| Model {
            id: id.clone(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: provider_map
                .get(&id)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
        })
        .collect();

    Ok(Json(ListModelsResponse {
        object: "list".to_string(),
        data: models,
    }))
}

/// 获取模型信息
/// GET /v1/models/{model}
///
/// 从数据库查询指定模型，返回其所属 Provider 信息
pub async fn retrieve_model(
    State(state): State<AppState>,
    Path(model_id): Path<String>,
) -> Result<Json<Model>> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::Internal("Database not configured".to_string()))?;

    // 查询所有启用的账号，找到支持该模型的 Provider
    let accounts = Account::find_enabled_all(pool)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to query accounts: {}", e)))?;

    for account in accounts {
        if account.models_supported.contains(&model_id) {
            return Ok(Json(Model {
                id: model_id,
                object: "model".to_string(),
                created: chrono::Utc::now().timestamp(),
                owned_by: account.provider,
            }));
        }
    }

    // 模型不存在
    Err(ApiError::NotFound(format!("Model not found: {}", model_id)))
}

/// 将节点的完整响应转换为模拟流式输出
///
/// 该函数接收节点返回的完整 ChatCompletionResponse，
/// 将其内容拆分为多个 SSE chunk，模拟 token 级流式输出。
fn simulate_node_stream(
    response: keycompute_types::ChatCompletionResponse,
    ctx: Arc<RequestContext>,
    model: String,
    stream_options: Option<StreamOptions>,
    lifecycle: Arc<dyn RequestLifecycleRecorder>,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    // 伪流式（simulated streaming）：
    // - Node 路径先通过 enqueue_and_wait() 获取完整响应
    // - 再将完整文本按字符拆分为 ~20 个块，每块间隔 10ms 发送
    // - 模拟真实 SSE 流式输出的用户体验
    //
    // 注：Node 响应目前仅包含单个 choice（n=1），多 choice 场景暂不支持。
    let (sse_tx, sse_rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let outcome = 'delivery: {
            let completion_id = generate_completion_id();
            let created = chrono::Utc::now().timestamp();
            let mut client_connected = true;

            // 获取第一个 choice 的文本内容
            let content = response
                .choices
                .first()
                .map(|c| c.message.content.clone())
                .unwrap_or_default();

            // 将内容拆分为字符级别的 chunk（模拟 token 级输出）
            // 注：这里是简单实现，按字符拆分，实际可以按 token 拆分
            let chars: Vec<char> = content.chars().collect();
            let chunk_size = std::cmp::max(1, chars.len() / 20); // 至少 1 个字符，最多 20 个 chunk

            // 发送 content chunks，仅首个 chunk 携带 role（遵循 OpenAI SSE 协议）
            let mut first_chunk = true;
            let mut client_first_content_recorded = false;
            for chunk in chars.chunks(chunk_size) {
                let chunk_content: String = chunk.iter().collect();
                let delta = if first_chunk {
                    first_chunk = false;
                    serde_json::json!({
                        "role": "assistant",
                        "content": chunk_content
                    })
                } else {
                    serde_json::json!({
                        "content": chunk_content
                    })
                };
                let data = serde_json::json!({
                    "id": completion_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": delta,
                        "finish_reason": null
                    }]
                });
                let sent = forward_openai_sse_event(
                    &sse_tx,
                    &ctx,
                    &mut client_connected,
                    Event::default().data(data.to_string()),
                )
                .await;
                if sent && !client_first_content_recorded {
                    if let Err(error) = lifecycle
                        .record_client_first_content(ctx.request_id, chrono::Utc::now())
                        .await
                    {
                        tracing::warn!(request_id=%ctx.request_id,%error,"failed to record Node client first content");
                    }
                    client_first_content_recorded = true;
                }
                if !client_connected {
                    break 'delivery ClientResponseOutcome::ClientDisconnected;
                }

                // 小延迟，模拟真实流式输出
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }

            // 发送最后一个带有 finish_reason 的 chunk
            let finish_reason = response
                .choices
                .first()
                .and_then(|c| c.finish_reason.clone())
                .unwrap_or("stop".to_string());
            let data = serde_json::json!({
                "id": completion_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": finish_reason
                }]
            });
            let _sent = forward_openai_sse_event(
                &sse_tx,
                &ctx,
                &mut client_connected,
                Event::default().data(data.to_string()),
            )
            .await;
            if !client_connected {
                break 'delivery ClientResponseOutcome::ClientDisconnected;
            }

            // 如果请求了 usage，发送 usage chunk
            if stream_options
                .as_ref()
                .map(|o| o.include_usage)
                .unwrap_or(false)
            {
                let data = serde_json::json!({
                    "id": completion_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [],
                    "usage": {
                        "prompt_tokens": response.usage.prompt_tokens,
                        "completion_tokens": response.usage.completion_tokens,
                        "total_tokens": response.usage.total_tokens
                    }
                });
                if !forward_openai_sse_event(
                    &sse_tx,
                    &ctx,
                    &mut client_connected,
                    Event::default().data(data.to_string()),
                )
                .await
                {
                    break 'delivery ClientResponseOutcome::ClientDisconnected;
                }
            }

            // 发送 [DONE] 标记，声明流式传输结束（OpenAI SSE 协议要求）
            if forward_openai_sse_event(
                &sse_tx,
                &ctx,
                &mut client_connected,
                Event::default().data("[DONE]"),
            )
            .await
            {
                ClientResponseOutcome::Succeeded
            } else {
                ClientResponseOutcome::ClientDisconnected
            }
        };
        super::finish_client_response_trace(&lifecycle, &ctx, outcome).await;
    });

    ReceiverStream::new(sse_rx).map(Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::time::Duration;

    #[test]
    fn chat_request_accepts_common_official_tool_fields() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "developer", "content": "Use tools when needed"},
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
            "tools": [{
                "type": "function",
                "function": {
                    "name": "weather",
                    "description": "Get weather",
                    "parameters": {"type": "object"}
                }
            }],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "prompt_cache_key": "cache-user-42",
            "safety_identifier": "safe-user-42",
            "max_completion_tokens": 128,
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "answer", "schema": {"type": "object"}}
            }
        });

        let request = parse_chat_completion_request(&body).unwrap();
        assert_eq!(request.messages[0].role, "developer");
        assert_eq!(request.effective_max_tokens(), Some(128));
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn chat_request_preserves_unknown_top_level_fields_for_native_forwarding() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "future_official_role", "content": "Hello"}],
            "top_k": 40,
            "enable_thinking": true
        });

        let request = parse_chat_completion_request(&body).unwrap();
        assert_eq!(body["top_k"], 40);
        assert_eq!(body["enable_thinking"], true);
        assert_eq!(body["messages"][0]["role"], "future_official_role");
        assert_eq!(
            project_chat_message_role(&request.messages[0].role),
            MessageRole::User
        );
    }

    #[test]
    fn chat_request_accepts_current_audio_file_and_custom_tool_shapes() {
        let body = serde_json::json!({
            "model": "gpt-4o-audio-preview",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "input_audio", "input_audio": {"data": "AA==", "format": "wav"}},
                    {"type": "file", "file": {"file_id": "file_123"}}
                ]
            }],
            "modalities": ["text", "audio"],
            "audio": {"voice": "alloy", "format": "wav"},
            "tools": [{"type": "custom", "custom": {"name": "shell", "format": {"type": "text"}}}],
            "tool_choice": {"type": "allowed_tools", "allowed_tools": {"mode": "auto", "tools": []}},
            "metadata": {"source": "compat-test"},
            "prediction": {"type": "content", "content": "expected"},
            "service_tier": "auto",
            "store": true,
            "verbosity": "low",
            "web_search_options": {}
        });

        let request = parse_chat_completion_request(&body).unwrap();
        assert_eq!(request.messages.len(), 1);
        assert!(
            request.messages[0]
                .content
                .extract_text()
                .contains("[input_audio]")
        );
        assert!(
            request.messages[0]
                .content
                .extract_text()
                .contains("[file]")
        );
    }

    #[test]
    fn chat_routing_projection_does_not_copy_large_inline_media() {
        let inline = "A".repeat(1024 * 1024);
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{inline}")}},
                    {"type": "input_audio", "input_audio": {"data": inline, "format": "wav"}}
                ]
            }]
        });

        let request = parse_chat_completion_request(&body).unwrap();
        let projected = request.messages[0].content.extract_text();
        assert!(projected.contains("[inline_image]"));
        assert!(projected.contains("[input_audio]"));
        assert!(projected.len() < 128);
        assert!(body.to_string().len() > 2 * 1024 * 1024);
    }

    #[test]
    fn node_chat_messages_restore_supported_inline_images_from_the_native_body() {
        let inline_url = "data:image/png;base64,AAEC";
        let body = serde_json::json!({
            "model": "node:vision-model",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "describe this"},
                    {"type": "image_url", "image_url": {"url": inline_url, "detail": "high"}}
                ]
            }]
        });

        let messages = node_chat_messages(&body).unwrap();
        let serialized = serde_json::to_value(messages).unwrap();
        assert_eq!(serialized[0]["content"][1]["image_url"]["url"], inline_url);
        assert_eq!(serialized[0]["content"][1]["image_url"]["detail"], "high");
    }

    #[test]
    fn node_chat_messages_reject_content_the_node_protocol_cannot_represent() {
        let body = serde_json::json!({
            "model": "node:audio-model",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "input_audio": {"data": "AA==", "format": "wav"}
                }]
            }]
        });

        let error = node_chat_messages(&body).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not supported by Node Chat tasks")
        );
    }

    #[test]
    fn non_stream_collector_keeps_native_chat_response() {
        let body = serde_json::json!({
            "id": "chatcmpl-native",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{"id": "call_1", "type": "function"}]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let mut collector = StreamCollector::new();
        assert!(
            collector
                .process_event(llm_protocol_provider::StreamEvent::native(
                    NativeStreamEvent::OpenAiChatJson {
                        body: body.clone(),
                        admission: None,
                    },
                ))
                .unwrap()
        );
        assert_eq!(
            collector
                .native_chat_response
                .as_ref()
                .map(|response| &response.body),
            Some(&body)
        );
    }

    #[test]
    fn native_chat_sse_uses_single_slot_client_backpressure() {
        let mut ctx = RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-4o",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        );
        assert_eq!(openai_sse_channel_capacity(&ctx), 100);

        ctx.native_openai_chat_request = Some(Arc::new(serde_json::json!({})));
        assert_eq!(
            openai_sse_channel_capacity(&ctx),
            LARGE_NATIVE_EVENT_CHANNEL_CAPACITY
        );
    }

    #[tokio::test]
    async fn native_chat_response_chunk_owns_its_admission_guard() {
        struct TestGuard(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for TestGuard {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guarded = retain_openai_bytes_guard(
            bytes::Bytes::from_static(b"large chat response"),
            TestGuard(Arc::clone(&released)),
        );
        let response = axum::response::Response::new(Body::from(guarded));
        let (head, body) = response.into_parts();
        drop(head);
        assert!(!released.load(std::sync::atomic::Ordering::SeqCst));

        let mut body = body.into_data_stream();
        let chunk = body.next().await.unwrap().unwrap();
        assert_eq!(chunk, bytes::Bytes::from_static(b"large chat response"));
        assert!(body.next().await.is_none());
        assert!(!released.load(std::sync::atomic::Ordering::SeqCst));

        drop(chunk);
        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn detached_generation_worker_retains_large_request_admission() {
        let state = AppState::new();
        let admission = Arc::clone(&state.generation_http_body_admission);
        let worker_permit = admission.try_acquire().unwrap();
        let second_permit = admission.try_acquire().unwrap();
        assert!(admission.try_acquire().is_none());

        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(retain_generation_body_permit(
            Some(worker_permit),
            async move {
                let _ = finish_rx.await;
            },
        ));
        tokio::task::yield_now().await;
        assert!(admission.try_acquire().is_none());

        finish_tx.send(()).unwrap();
        worker.await.unwrap();
        assert!(admission.try_acquire().is_some());
        drop(second_permit);
    }

    #[tokio::test]
    async fn chat_terminal_billing_records_tpm_once() {
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
        ctx.set_input_tokens(9);
        ctx.set_output_tokens(4);
        let rate_limiter = Arc::new(keycompute_ratelimit::RateLimitService::default_memory());
        let settlement = super::ImmediateSettlementServices {
            billing: Arc::new(keycompute_billing::BillingService::new()),
            rate_limiter: Arc::clone(&rate_limiter),
            durable_state: None,
        };
        let account_id = uuid::Uuid::new_v4();

        finalize_openai_billing(&settlement, &ctx, "openai", account_id, "success").await;
        finalize_openai_billing(&settlement, &ctx, "openai", account_id, "success").await;

        let key = keycompute_ratelimit::RateLimitKey::new(
            ctx.tenant_id,
            ctx.user_id,
            ctx.produce_ai_key_id,
        );
        assert_eq!(rate_limiter.get_tpm_count(&key).await.unwrap(), 13);
    }

    #[tokio::test]
    async fn chat_completion_trace_preserves_ingress_received_at() {
        let received_at = chrono::DateTime::parse_from_rfc3339("2025-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let recorder = Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let mut state = AppState::with_config(crate::state::AppStateConfig::default());
        state.lifecycle = Arc::clone(&recorder) as Arc<dyn RequestLifecycleRecorder>;
        let auth = AuthExtractor::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "user",
        );
        let request = serde_json::from_value(serde_json::json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .unwrap();

        let result = chat_completions(
            State(state),
            auth,
            RequestId::new(),
            ClientRequestId(None),
            RequestReceivedAt(received_at),
            (None, Json(request)),
        )
        .await;

        assert!(matches!(result, Err(ApiError::Forbidden(_))));
        let starts = recorder.request_starts();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].protocol, "openai");
        assert_eq!(starts[0].received_at, received_at);
    }

    #[test]
    fn node_route_stays_routing_until_task_creation_succeeds() {
        assert_eq!(
            initial_route_trace_state(&ExecutionTarget::new_node("node-model")),
            (RouteType::Node, RequestStatus::Routing)
        );
        assert_eq!(
            initial_route_trace_state(&ExecutionTarget::new_provider(
                "openai",
                uuid::Uuid::new_v4(),
                "https://provider.example/v1",
                "secret",
            )),
            (RouteType::ProviderAccount, RequestStatus::Routing)
        );
    }

    fn node_test_response(content: &str) -> keycompute_types::ChatCompletionResponse {
        keycompute_types::ChatCompletionResponse {
            id: "node-response".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "node-model".to_string(),
            choices: vec![keycompute_types::response::CompletionChoice {
                index: 0,
                message: keycompute_types::response::ResponseMessage {
                    role: "assistant".to_string(),
                    content: content.to_string(),
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: keycompute_types::Usage {
                prompt_tokens: 2,
                completion_tokens: 3,
                total_tokens: 5,
            },
        }
    }

    fn node_stream_test_context() -> Arc<RequestContext> {
        Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "node-model",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ))
    }

    #[tokio::test]
    async fn node_simulated_stream_finishes_request_after_done() {
        let ctx = node_stream_test_context();
        let recorder = Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let stream = simulate_node_stream(
            node_test_response("complete response"),
            Arc::clone(&ctx),
            "node-model".to_string(),
            Some(StreamOptions {
                include_usage: true,
            }),
            Arc::clone(&recorder) as Arc<dyn RequestLifecycleRecorder>,
        );

        let events: Vec<_> = stream.collect().await;
        assert!(
            events.len() >= 3,
            "content, terminal, usage, and DONE frames"
        );
        assert_eq!(
            ctx.client_response_outcome(),
            Some(ClientResponseOutcome::Succeeded)
        );
        let finishes = recorder.request_finishes();
        assert_eq!(finishes.len(), 1);
        assert_eq!(finishes[0].status, RequestStatus::Succeeded);
        assert!(finishes[0].error.is_none());
    }

    #[tokio::test]
    async fn node_empty_simulated_stream_does_not_record_client_first_content() {
        let ctx = node_stream_test_context();
        let recorder = Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let stream = simulate_node_stream(
            node_test_response(""),
            Arc::clone(&ctx),
            "node-model".to_string(),
            None,
            Arc::clone(&recorder) as Arc<dyn RequestLifecycleRecorder>,
        );

        let _events: Vec<_> = stream.collect().await;
        assert_eq!(
            ctx.client_response_outcome(),
            Some(ClientResponseOutcome::Succeeded)
        );
        assert!(
            recorder
                .events()
                .iter()
                .all(|event| !event.starts_with("client_first_content:"))
        );
    }

    #[tokio::test]
    async fn node_simulated_stream_disconnect_cancels_request_trace() {
        let ctx = node_stream_test_context();
        let recorder = Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let stream = simulate_node_stream(
            node_test_response("not delivered"),
            Arc::clone(&ctx),
            "node-model".to_string(),
            None,
            Arc::clone(&recorder) as Arc<dyn RequestLifecycleRecorder>,
        );
        drop(stream);

        tokio::time::timeout(Duration::from_secs(1), async {
            while recorder.request_finishes().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("disconnect should promptly finish the Node request trace");

        assert_eq!(
            ctx.client_response_outcome(),
            Some(ClientResponseOutcome::ClientDisconnected)
        );
        let finishes = recorder.request_finishes();
        assert_eq!(finishes.len(), 1);
        assert_eq!(finishes[0].status, RequestStatus::Cancelled);
        let error = finishes[0].error.as_ref().expect("disconnect error");
        assert_eq!(error.category, TraceErrorCategory::ClientDisconnect);
        assert!(
            recorder
                .events()
                .iter()
                .all(|event| !event.starts_with("client_first_content:"))
        );
    }

    #[tokio::test]
    async fn dropping_node_wait_guard_cancels_request_trace() {
        let ctx = node_stream_test_context();
        let recorder = Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let guard = crate::handlers::ClientResponseGuard::new(
            Arc::clone(&recorder) as Arc<dyn RequestLifecycleRecorder>,
            Arc::clone(&ctx),
        );

        // This models Axum dropping the handler future while it is awaiting a
        // Node result. The guard is the only remaining cleanup opportunity.
        drop(guard);

        tokio::time::timeout(Duration::from_secs(1), async {
            while recorder.request_finishes().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping the Node wait guard should promptly finish the trace");

        assert!(ctx.is_client_disconnected());
        assert_eq!(
            ctx.client_response_outcome(),
            Some(ClientResponseOutcome::ClientDisconnected)
        );
        let finishes = recorder.request_finishes();
        assert_eq!(finishes.len(), 1);
        assert_eq!(finishes[0].status, RequestStatus::Cancelled);
        assert_eq!(
            finishes[0].error.as_ref().map(|error| error.category),
            Some(TraceErrorCategory::ClientDisconnect)
        );
    }

    #[tokio::test]
    async fn disarmed_node_wait_guard_leaves_completion_to_response_path() {
        let ctx = node_stream_test_context();
        let recorder = Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let mut guard = crate::handlers::ClientResponseGuard::new(
            Arc::clone(&recorder) as Arc<dyn RequestLifecycleRecorder>,
            Arc::clone(&ctx),
        );
        guard.disarm();
        drop(guard);

        tokio::task::yield_now().await;
        assert!(!ctx.is_client_disconnected());
        assert!(recorder.request_finishes().is_empty());
    }

    #[test]
    fn test_chat_completion_request_deserialize() {
        let json = r#"{
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "temperature": 0.7,
            "max_tokens": 100
        }"#;
        let body: Value = serde_json::from_str(json).unwrap();
        let req = parse_chat_completion_request(&body).unwrap();
        assert_eq!(req.model, "gpt-4o");
        assert!(!req.stream);
        assert_eq!(req.temperature, Some(0.7));
    }

    #[test]
    fn test_chat_completion_stream_request() {
        let json = r#"{
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true,
            "stream_options": {"include_usage": true}
        }"#;
        let body: Value = serde_json::from_str(json).unwrap();
        let req = parse_chat_completion_request(&body).unwrap();
        assert!(req.stream);
        assert!(req.stream_options.unwrap().include_usage);
    }

    #[tokio::test]
    async fn openai_stream_waits_for_done_after_finish_reason() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "claude-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let mut stream = Box::pin(create_openai_stream(
            rx,
            Arc::clone(&ctx),
            "claude-test".to_string(),
            "anthropic".to_string(),
            uuid::Uuid::new_v4(),
            Arc::new(keycompute_billing::BillingService::new()),
            None,
        ));

        tx.send(llm_protocol_provider::StreamEvent::Delta {
            content: String::new(),
            finish_reason: Some("stop".to_string()),
        })
        .await
        .unwrap();
        assert!(
            stream
                .next()
                .await
                .expect("finish_reason delta should be forwarded")
                .is_ok()
        );

        // 若 handler 在 finish_reason 后提前终止，后续 delta 会丢失。executor 在
        // Done 之前仍可能发送 Usage 与更多 delta，流必须保持打开；用“第二帧仍被
        // 转发”做确定性断言，替代固定时长的负向等待（原 25ms 断言易 flaky）。
        tx.send(llm_protocol_provider::StreamEvent::Delta {
            content: "tail".to_string(),
            finish_reason: None,
        })
        .await
        .unwrap();
        assert!(
            stream
                .next()
                .await
                .expect("stream must stay open and forward the post-finish_reason delta")
                .is_ok()
        );

        ctx.set_input_tokens(7);
        ctx.set_output_tokens(3);
        tx.send(llm_protocol_provider::StreamEvent::Usage {
            input_tokens: 7,
            output_tokens: 3,
        })
        .await
        .unwrap();
        tx.send(llm_protocol_provider::StreamEvent::Done)
            .await
            .unwrap();
        drop(tx);

        // Usage 不产生 SSE 帧（由 executor 经 ctx 消费）；Done 后输出 [DONE] 并关闭
        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn openai_stream_incomplete_path_emits_error_frame() {
        // channel 在 Done 之前关闭（上游中断/传输层截断）时，流必须以显式错误
        // 帧结束，不能静默截断：否则客户端会把截断的流误认为完整响应。
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "claude-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let stream = Box::pin(create_openai_stream(
            rx,
            ctx,
            "claude-test".to_string(),
            "anthropic".to_string(),
            uuid::Uuid::new_v4(),
            Arc::new(keycompute_billing::BillingService::new()),
            None,
        ));
        drop(tx);

        let response = Sse::new(stream).into_response();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("Stream ended unexpectedly"),
            "incomplete stream must surface a generic error, got: {body}"
        );
        // 与 keepalive 变体一致，错误帧后必须以 [DONE] 终止。
        assert!(body.contains("[DONE]"));
    }

    #[tokio::test]
    async fn openai_stream_keepalive_incomplete_path_emits_error_frame() {
        // keepalive 变体同样不得静默截断：channel 在 Done 之前关闭时必须输出
        // 显式错误帧与 [DONE] 终止符。
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "claude-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let stream = Box::pin(create_openai_stream_with_keepalive_and_lifecycle(
            rx,
            OpenAiStreamContext {
                ctx,
                model: "claude-test".to_string(),
                provider_name: "anthropic".to_string(),
                account_id: uuid::Uuid::new_v4(),
                settlement: super::ImmediateSettlementServices::for_test(Arc::new(
                    keycompute_billing::BillingService::new(),
                )),
                stream_options: None,
                lifecycle: Arc::new(keycompute_types::NoopRequestLifecycleRecorder),
                initial_event: None,
                initial_status: None,
                body_permit: None,
            },
            Duration::from_secs(120),
        ));
        drop(tx);

        let response = Sse::new(stream).into_response();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("Stream ended unexpectedly"),
            "incomplete keepalive stream must surface a generic error, got: {body}"
        );
        assert!(body.contains("[DONE]"));
    }

    #[tokio::test(start_paused = true)]
    async fn accepted_multimodal_stream_emits_keepalive_before_first_provider_event() {
        let (_upstream_tx, upstream_rx) = tokio::sync::mpsc::channel(4);
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
        let (initial_status_tx, initial_status_rx) = tokio::sync::oneshot::channel();
        let stream = create_openai_stream_with_keepalive_and_lifecycle(
            upstream_rx,
            OpenAiStreamContext {
                ctx: Arc::clone(&ctx),
                model: "gpt-test".to_string(),
                provider_name: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                settlement: super::ImmediateSettlementServices::for_test(Arc::new(
                    keycompute_billing::BillingService::new(),
                )),
                stream_options: None,
                lifecycle: Arc::new(keycompute_types::NoopRequestLifecycleRecorder),
                initial_event: None,
                initial_status: Some(initial_status_tx),
                body_permit: None,
            },
            Duration::from_secs(60),
        );

        ctx.mark_upstream_response_accepted();
        assert_eq!(
            crate::handlers::await_initial_stream_status(&ctx, initial_status_rx).await,
            crate::handlers::InitialStreamStatus::Ready
        );

        let response = Sse::new(stream).into_response();
        let mut body = response.into_body().into_data_stream();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        let chunk = body
            .next()
            .await
            .expect("keepalive body must remain open")
            .expect("keepalive frame must be valid");
        assert!(!chunk.is_empty(), "expected SSE keepalive bytes");
    }

    #[tokio::test(start_paused = true)]
    async fn openai_stream_keepalive_honors_configured_timeout_and_emits_terminal_error() {
        // 配置为 180s 时，旧的 120s 硬上限不能提前截断响应；到达配置期限后
        // 必须以显式错误帧 + [DONE] 终止，并将结算状态置为 timeout。
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "claude-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let stream = Box::pin(create_openai_stream_with_keepalive_and_lifecycle(
            rx,
            OpenAiStreamContext {
                ctx: Arc::clone(&ctx),
                model: "claude-test".to_string(),
                provider_name: "anthropic".to_string(),
                account_id: uuid::Uuid::new_v4(),
                settlement: super::ImmediateSettlementServices::for_test(Arc::new(
                    keycompute_billing::BillingService::new(),
                )),
                stream_options: None,
                lifecycle: Arc::new(keycompute_types::NoopRequestLifecycleRecorder),
                initial_event: None,
                initial_status: None,
                body_permit: None,
            },
            Duration::from_secs(180),
        ));
        // channel 保持打开且无事件：rx.recv() 挂起，只有 deadline 能触发终止
        let _keep_tx_alive = tx;

        let app_handle = tokio::spawn(async move {
            let response = Sse::new(stream).into_response();
            axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap()
        });

        // 超过旧硬上限后仍应继续等待上游。
        for _ in 0..1300 {
            if app_handle.is_finished() {
                break;
            }
            tokio::time::advance(Duration::from_millis(100)).await;
        }
        assert!(
            !app_handle.is_finished(),
            "configured 180s timeout must not terminate at the old 120s cap"
        );

        // 推进到配置的 180s deadline，触发错误帧与 [DONE]。
        for _ in 0..600 {
            if app_handle.is_finished() {
                break;
            }
            tokio::time::advance(Duration::from_millis(100)).await;
        }

        let body = tokio::time::timeout(Duration::from_secs(1), app_handle)
            .await
            .expect("keepalive stream must terminate after the timeout branch")
            .expect("body collection should succeed");
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains("Request timed out"),
            "timeout branch must surface the explicit timeout error, got: {body}"
        );
        assert!(body.contains("[DONE]"), "timeout must end with [DONE]");
        assert!(
            !body.contains("data: {\"choices\""),
            "no content chunks may be emitted before the timeout"
        );
    }

    #[tokio::test]
    async fn openai_plain_non_streaming_worker_survives_handler_cancellation() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-4o",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        ));
        let handler = tokio::spawn(create_openai_response_with_lifecycle(
            rx,
            OpenAiJsonRuntime {
                ctx: Arc::clone(&ctx),
                model: "gpt-4o".to_string(),
                provider_name: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                settlement: super::ImmediateSettlementServices::for_test(Arc::new(
                    keycompute_billing::BillingService::new(),
                )),
                lifecycle: Arc::new(keycompute_types::NoopRequestLifecycleRecorder),
                body_permit: None,
            },
        ));
        tokio::task::yield_now().await;
        handler.abort();
        let _ = handler.await;

        tx.send(llm_protocol_provider::StreamEvent::error(
            "client disconnected",
        ))
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), tx.closed())
            .await
            .expect("detached worker must consume the terminal event after handler cancellation");

        assert!(ctx.is_client_disconnected());
        assert_eq!(
            ctx.client_response_outcome(),
            Some(ClientResponseOutcome::ClientDisconnected)
        );
    }

    #[tokio::test]
    async fn openai_stream_error_redacts_upstream_message() {
        // 流式错误事件中的上游消息绝不能原样进入 SSE：客户端只能看到
        // 通用错误文本，原始消息保留在服务端日志（与 Anthropic 路径一致）。
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "claude-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let stream = Box::pin(create_openai_stream(
            rx,
            Arc::clone(&ctx),
            "claude-test".to_string(),
            "anthropic".to_string(),
            uuid::Uuid::new_v4(),
            Arc::new(keycompute_billing::BillingService::new()),
            None,
        ));

        tx.send(llm_protocol_provider::StreamEvent::error(
            "upstream-secret-detail",
        ))
        .await
        .unwrap();
        drop(tx);

        let response = Sse::new(stream).into_response();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Upstream request failed"));
        assert!(!body.contains("upstream-secret-detail"));
        // 错误帧后以 [DONE] 终止，与 keepalive 变体一致。
        assert!(body.contains("[DONE]"));
    }

    #[tokio::test]
    async fn openai_stream_worker_settles_cancellation_after_client_disconnect() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-4o",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let (initial_status_tx, initial_status_rx) = tokio::sync::oneshot::channel();
        let stream = create_openai_stream_with_lifecycle(
            rx,
            OpenAiStreamContext {
                ctx: Arc::clone(&ctx),
                model: "gpt-4o".to_string(),
                provider_name: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                settlement: super::ImmediateSettlementServices::for_test(Arc::new(
                    keycompute_billing::BillingService::new(),
                )),
                stream_options: None,
                lifecycle: Arc::new(keycompute_types::NoopRequestLifecycleRecorder),
                initial_event: None,
                initial_status: Some(initial_status_tx),
                body_permit: None,
            },
        );
        drop(stream);

        // executor 观察到 RequestContext 的取消令牌后会丢弃当前上游流，
        // 再用终止 Error 唤醒仍持有 receiver 的结算 worker。
        tx.send(llm_protocol_provider::StreamEvent::error(
            "client disconnected",
        ))
        .await
        .unwrap();

        assert_eq!(
            initial_status_rx.await.unwrap(),
            crate::handlers::InitialStreamStatus::Failed
        );

        tokio::time::timeout(Duration::from_secs(1), tx.closed())
            .await
            .expect("worker must consume cancellation and finish settlement after disconnect");
        assert!(ctx.is_client_disconnected());
    }

    #[tokio::test]
    async fn failed_openai_sse_send_does_not_record_client_first_content() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let ctx = Arc::new(RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-4o",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        ));
        let recorder = Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let stream = create_openai_stream_with_lifecycle(
            rx,
            OpenAiStreamContext {
                ctx: Arc::clone(&ctx),
                model: "gpt-4o".to_string(),
                provider_name: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                settlement: super::ImmediateSettlementServices::for_test(Arc::new(
                    keycompute_billing::BillingService::new(),
                )),
                stream_options: None,
                lifecycle: Arc::clone(&recorder) as Arc<dyn RequestLifecycleRecorder>,
                initial_event: None,
                initial_status: None,
                body_permit: None,
            },
        );
        drop(stream);

        tx.send(llm_protocol_provider::StreamEvent::Delta {
            content: "not delivered".to_string(),
            finish_reason: None,
        })
        .await
        .unwrap();
        tx.send(llm_protocol_provider::StreamEvent::Done)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), tx.closed())
            .await
            .expect("worker should settle and stop after the failed client send");

        assert!(ctx.is_client_disconnected());
        assert!(
            recorder
                .events()
                .iter()
                .all(|event| !event.starts_with("client_first_content:"))
        );
    }

    fn minimal_request(extra: &str) -> ChatCompletionRequest {
        let json = format!(
            r#"{{
                "model": "gpt-4o",
                "messages": [{{"role": "user", "content": "Hello"}}]{}{}
            }}"#,
            if extra.is_empty() { "" } else { "," },
            extra
        );
        let body: Value = serde_json::from_str(&json).unwrap();
        parse_chat_completion_request(&body).unwrap()
    }

    #[test]
    fn test_validate_sampling_params_in_range() {
        assert!(minimal_request("").validate_sampling_params().is_ok());
        assert!(
            minimal_request(r#""max_tokens": 100, "temperature": 2.0, "top_p": 1.0"#)
                .validate_sampling_params()
                .is_ok()
        );
        assert!(
            minimal_request(r#""temperature": 0.0, "top_p": 0.0"#)
                .validate_sampling_params()
                .is_ok()
        );
    }

    #[test]
    fn test_validate_sampling_params_out_of_range() {
        // 越界参数应在 handler 层拒绝，不进入路由/上游调用
        for extra in [
            r#""max_tokens": 0"#,
            r#""max_completion_tokens": 0"#,
            r#""temperature": -0.1"#,
            r#""temperature": 2.1"#,
            r#""top_p": -0.1"#,
            r#""top_p": 1.5"#,
        ] {
            assert!(
                minimal_request(extra).validate_sampling_params().is_err(),
                "{extra} should be rejected"
            );
        }
    }

    #[test]
    fn test_max_completion_tokens_alias() {
        // 新版字段 max_completion_tokens 作为 max_tokens 的回退别名
        let req = minimal_request(r#""max_completion_tokens": 256"#);
        assert_eq!(req.effective_max_tokens(), Some(256));

        // 两者同时提供时 max_tokens 优先
        let req = minimal_request(r#""max_tokens": 100, "max_completion_tokens": 256"#);
        assert_eq!(req.effective_max_tokens(), Some(100));
    }

    #[test]
    fn test_tool_call_serialization() {
        let tool_call = ToolCall {
            id: "call_123".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "get_weather".to_string(),
                arguments: r#"{"location": "Beijing"}"#.to_string(),
            },
        };
        let json = serde_json::to_string(&tool_call).unwrap();
        assert!(json.contains("call_123"));
        assert!(json.contains("get_weather"));
    }

    #[tokio::test]
    async fn test_list_models() {
        // 测试模型结构序列化
        let model = Model {
            id: "gpt-4o".to_string(),
            object: "model".to_string(),
            created: chrono::Utc::now().timestamp(),
            owned_by: "openai".to_string(),
        };
        let json = serde_json::to_string(&model).unwrap();
        assert!(json.contains("gpt-4o"));
        assert!(json.contains("model"));
    }

    // 注意：retrieve_model 需要 AppState 和数据库连接，
    // 适合在集成测试中测试，这里不再单独测试

    #[test]
    fn test_has_image_content_empty() {
        assert!(!has_image_content(&[]));
    }

    #[test]
    fn test_has_image_content_text_only() {
        let msg = Message::new(MessageRole::User, MessageContent::text("Hello"));
        assert!(!has_image_content(&[msg]));
    }

    #[test]
    fn test_has_image_content_text_parts() {
        let msg = Message {
            role: MessageRole::User,
            content: MessageContent::Parts(vec![ContentPart::Text {
                text: "Hello".to_string(),
            }]),
        };
        assert!(!has_image_content(&[msg]));
    }

    #[test]
    fn test_has_image_content_with_image_url() {
        use keycompute_types::ImageUrl;
        let msg = Message {
            role: MessageRole::User,
            content: MessageContent::Parts(vec![ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "https://example.com/image.png".to_string(),
                    detail: None,
                },
            }]),
        };
        assert!(has_image_content(&[msg]));
    }

    #[test]
    fn test_has_image_content_mixed_parts() {
        use keycompute_types::ImageUrl;
        let msg = Message {
            role: MessageRole::User,
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "Describe this".to_string(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "https://example.com/photo.jpg".to_string(),
                        detail: None,
                    },
                },
            ]),
        };
        assert!(has_image_content(&[msg]));
    }

    #[test]
    fn test_has_image_content_data_uri() {
        use keycompute_types::ImageUrl;
        let msg = Message {
            role: MessageRole::User,
            content: MessageContent::Parts(vec![ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,iVBORw0KGgo...".to_string(),
                    detail: None,
                },
            }]),
        };
        assert!(!has_image_content(&[msg]));
    }

    fn test_account(provider: &str, models: &[&str]) -> Account {
        let now = chrono::Utc::now();
        Account {
            id: uuid::Uuid::new_v4(),
            tenant_id: uuid::Uuid::new_v4(),
            provider: provider.to_string(),
            name: format!("{provider}-account"),
            endpoint: "https://example.com/v1".to_string(),
            upstream_api_key_encrypted: "sk-encrypted".to_string(),
            upstream_api_key_preview: "sk-t****".to_string(),
            rpm_limit: 60,
            tpm_limit: 100_000,
            priority: 10,
            enabled: true,
            models_supported: models.iter().map(|m| m.to_string()).collect(),
            api_capabilities: if provider == "anthropic" {
                vec!["messages".to_string()]
            } else {
                vec!["chat_completions".to_string(), "responses".to_string()]
            },
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
    fn list_models_query_defaults_to_openai_protocol() {
        // 缺省 protocol 时按 openai 入口过滤（与 /v1/chat/completions 的隔离一致）
        let json = serde_json::json!({});
        let query: ListModelsQuery = serde_json::from_value(json).unwrap();
        assert!(query.protocol.is_none());
        assert!(query.capability.is_none());

        // 显式指定入口协议
        let json = serde_json::json!({ "protocol": "anthropic" });
        let query: ListModelsQuery = serde_json::from_value(json).unwrap();
        assert_eq!(query.protocol.as_deref(), Some("anthropic"));
    }

    #[test]
    fn resolve_list_protocol_normalizes_case_and_rejects_unknown() {
        // 缺省按 openai 过滤
        assert_eq!(resolve_list_protocol(None).unwrap(), "openai");
        assert_eq!(resolve_list_protocol(Some("openai")).unwrap(), "openai");
        assert_eq!(
            resolve_list_protocol(Some("anthropic")).unwrap(),
            "anthropic"
        );
        // 大小写不敏感：parse 接受 "OpenAI"，规范化后仍与账号 provider（小写）匹配
        assert_eq!(resolve_list_protocol(Some("OpenAI")).unwrap(), "openai");
        assert_eq!(
            resolve_list_protocol(Some("ANTHROPIC")).unwrap(),
            "anthropic"
        );
        // 非协议名拒绝（避免过滤出空列表造成"静默无模型"）
        assert!(resolve_list_protocol(Some("deepseek")).is_err());
        assert!(resolve_list_protocol(Some("")).is_err());
    }

    #[test]
    fn collect_models_by_protocol_only_includes_matching_accounts() {
        let accounts = vec![
            test_account("openai", &["gpt-4o", "deepseek-chat"]),
            test_account("anthropic", &["claude-3-5-sonnet-20241022"]),
        ];

        // openai 入口：不包含 anthropic 账号声明的模型（否则列表与可调用性不一致）
        let (openai_models, openai_providers) =
            collect_models_by_protocol(accounts.clone(), "openai", None);
        assert!(openai_models.contains("gpt-4o"));
        assert!(openai_models.contains("deepseek-chat"));
        assert!(!openai_models.contains("claude-3-5-sonnet-20241022"));
        assert_eq!(
            openai_providers.get("gpt-4o").map(String::as_str),
            Some("openai")
        );

        // anthropic 入口：只包含 anthropic 账号声明的模型
        let (anthropic_models, anthropic_providers) =
            collect_models_by_protocol(accounts, "anthropic", None);
        assert!(anthropic_models.contains("claude-3-5-sonnet-20241022"));
        assert!(!anthropic_models.contains("gpt-4o"));
        assert_eq!(
            anthropic_providers
                .get("claude-3-5-sonnet-20241022")
                .map(String::as_str),
            Some("anthropic")
        );
    }

    #[test]
    fn collect_models_by_protocol_empty_without_matching_accounts() {
        let accounts = vec![test_account("anthropic", &["claude-opus-4"])];
        let (models, providers) = collect_models_by_protocol(accounts, "openai", None);
        assert!(models.is_empty());
        assert!(providers.is_empty());
    }

    #[test]
    fn collect_models_by_protocol_deduplicates_models_across_accounts() {
        // 同一协议下多个账号声明相同模型：列表去重，不产生重复条目
        let accounts = vec![
            test_account("openai", &["gpt-4o", "deepseek-chat"]),
            test_account("openai", &["gpt-4o", "deepseek-chat"]),
            test_account("openai", &["gpt-4o"]),
        ];
        let (models, providers) = collect_models_by_protocol(accounts, "openai", None);
        assert_eq!(models.len(), 2);
        assert!(models.contains("gpt-4o"));
        assert!(models.contains("deepseek-chat"));
        assert_eq!(providers.get("gpt-4o").map(String::as_str), Some("openai"));
        assert_eq!(providers.len(), 2);
    }

    #[test]
    fn collect_models_filters_by_api_capability() {
        let mut chat_only = test_account("openai", &["chat-model"]);
        chat_only.api_capabilities = vec!["chat_completions".to_string()];
        let mut responses_only = test_account("openai", &["responses-model"]);
        responses_only.api_capabilities = vec!["responses".to_string()];

        let (models, _) = collect_models_by_protocol(
            vec![chat_only, responses_only],
            "openai",
            Some(AccountApiCapability::Responses),
        );
        assert_eq!(
            models,
            std::collections::HashSet::from(["responses-model".to_string()])
        );
    }

    #[test]
    fn list_model_capability_must_match_protocol() {
        assert_eq!(
            resolve_list_capability("openai", Some("responses")).unwrap(),
            Some(AccountApiCapability::Responses)
        );
        assert!(resolve_list_capability("anthropic", Some("responses")).is_err());
        assert!(resolve_list_capability("openai", Some("messages")).is_err());
        assert!(resolve_list_capability("openai", Some("unknown")).is_err());
    }
}
