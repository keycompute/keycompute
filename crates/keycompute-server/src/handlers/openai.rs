//! OpenAI 兼容 API 处理器
//
//! 提供与 OpenAI API 完全兼容的接口
//! 参考: https://platform.openai.com/docs/api-reference

use crate::{
    error::{ApiError, Result},
    extractors::{AuthExtractor, RequestId},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
};
use futures::stream::Stream;
use keycompute_db::models::account::Account;
use keycompute_types::{ExecutionTarget, Message, MessageRole, RequestContext};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;

// ==================== Chat Completions ====================

/// Chat Completions 请求
/// 与 OpenAI API 完全对齐: https://platform.openai.com/docs/api-reference/chat/create
#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    /// 模型 ID (必需)
    pub model: String,
    /// 消息列表 (必需)
    pub messages: Vec<ChatCompletionMessage>,
    /// 是否流式输出 (默认 false)
    #[serde(default)]
    pub stream: bool,
    /// 最大生成 token 数
    #[serde(rename = "max_tokens")]
    pub max_tokens: Option<u32>,
    /// 温度参数 (0-2)
    pub temperature: Option<f32>,
    /// 核采样参数 (0-1)
    pub top_p: Option<f32>,
    /// 每个提示生成的结果数 (默认 1)
    #[serde(default = "default_n")]
    pub n: Option<u32>,
    /// 是否返回输入 token 的用量
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
    /// 停止序列
    pub stop: Option<StopSequence>,
    /// 存在惩罚 (-2.0 到 2.0)
    pub presence_penalty: Option<f32>,
    /// 频率惩罚 (-2.0 到 2.0)
    pub frequency_penalty: Option<f32>,
    /// 日志概率 (0-5)
    pub logprobs: Option<bool>,
    /// 返回的日志概率选项数
    pub top_logprobs: Option<u32>,
    /// 用户标识 (用于监控滥用)
    pub user: Option<String>,
    /// 响应格式 (如 json_object)
    pub response_format: Option<ResponseFormat>,
    /// 种子值 (用于可重复的结果)
    pub seed: Option<i64>,
    /// 工具列表
    pub tools: Option<Vec<Tool>>,
    /// 工具选择策略
    pub tool_choice: Option<ToolChoice>,
}

fn default_n() -> Option<u32> {
    Some(1)
}

/// Chat Completion 消息
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatCompletionMessage {
    /// 角色: system, user, assistant, tool
    pub role: String,
    /// 内容
    pub content: Option<String>,
    /// 工具调用 (assistant 消息中)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 工具调用 ID (tool 消息中)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 名称 (function 消息中)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 工具定义
#[derive(Debug, Deserialize)]
pub struct Tool {
    /// 工具类型 (目前只有 function)
    #[serde(rename = "type")]
    pub tool_type: String,
    /// 函数定义
    pub function: FunctionDefinition,
}

/// 函数定义
#[derive(Debug, Deserialize)]
pub struct FunctionDefinition {
    /// 函数名称
    pub name: String,
    /// 函数描述
    pub description: Option<String>,
    /// 参数定义 (JSON Schema)
    pub parameters: serde_json::Value,
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

/// 工具选择
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// 字符串选项: none, auto, required
    String(String),
    /// 指定调用特定函数
    Object {
        #[serde(rename = "type")]
        tool_type: String,
        function: FunctionChoice,
    },
}

/// 函数选择
#[derive(Debug, Deserialize)]
pub struct FunctionChoice {
    pub name: String,
}

/// 流式选项
#[derive(Debug, Deserialize)]
pub struct StreamOptions {
    /// 在流式消息的最后包含用量信息
    #[serde(default)]
    pub include_usage: bool,
}

/// 停止序列
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum StopSequence {
    /// 单个字符串
    String(String),
    /// 字符串数组 (最多 4 个)
    Array(Vec<String>),
}

/// 响应格式
#[derive(Debug, Deserialize)]
pub struct ResponseFormat {
    /// 格式类型: text 或 json_object
    #[serde(rename = "type")]
    pub format_type: String,
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

/// Chat Completions 处理器
/// POST /v1/chat/completions
///
/// 注意：限流已在中间件层统一处理，此处直接开始业务逻辑
pub async fn chat_completions(
    State(state): State<AppState>,
    auth: AuthExtractor,
    request_id: RequestId,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<axum::response::Response> {
    // 0. 余额预检查
    // 如果余额低于阈值（0.1元），直接拒绝请求
    if let Some(balance_service) = state.billing.balance_service() {
        balance_service
            .check_balance_for_tenant(auth.user_id, auth.tenant_id)
            .await
            .map_err(ApiError::from)?;
    }

    // 1. 构建 PricingSnapshot
    // 注意：此时 provider 尚未确定（路由在之后执行）
    // Node 模型（node:前缀）使用 empty provider，其他使用 openai
    let provider = keycompute_pricing::resolve_pricing_provider(&request.model);
    let pricing = state
        .pricing
        .create_snapshot(&request.model, &auth.tenant_id, Some(provider))
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create pricing snapshot: {}", e)))?;

    // 3. 转换消息格式
    let messages: Vec<Message> = request
        .messages
        .iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "system" => MessageRole::System,
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "tool" => MessageRole::Tool,
                _ => MessageRole::User, // 默认角色
            };
            Message {
                role,
                content: m.content.clone().unwrap_or_default(),
            }
        })
        .collect();

    // 4. 构建 RequestContext
    let mut ctx = Arc::new(RequestContext::new(
        auth.user_id,
        auth.tenant_id,
        auth.produce_ai_key_id,
        request.model.clone(),
        messages,
        request.stream,
        pricing,
    ));

    // 5. 智能路由
    let plan = state
        .routing
        .route(&ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("Routing failed: {}", e)))?;

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
            let node_gateway = state
                .node_gateway
                .as_ref()
                .ok_or_else(|| ApiError::Internal("node gateway not configured".to_string()))?;

            // 构建 NodeTaskPayload
            let payload = keycompute_types::node::NodeTaskPayload {
                request_id: ctx.request_id,
                chat: keycompute_types::ChatCompletionRequest {
                    model: model.clone(), // 使用去掉 node: 前缀的实际模型名
                    messages: ctx.messages.clone(),
                    stream: Some(request.stream), // 传递 stream 标志
                    max_tokens: request.max_tokens,
                    temperature: request.temperature,
                    top_p: request.top_p,
                    n: request.n,
                    stop: None, // StopSequence 不支持 Clone，暂时使用 None
                },
            };

            if request.stream {
                // 流式路径：获取完整响应后模拟流式输出
                let response = node_gateway
                    .enqueue_and_wait(auth.user_id, model.clone(), payload)
                    .await
                    .map_err(ApiError::from)?;

                // 更新 token 计数到 ctx（用于计费）
                ctx.set_input_tokens(response.usage.prompt_tokens);
                ctx.add_output_tokens(response.usage.completion_tokens);

                // 将完整响应转换为模拟流式输出
                let stream = simulate_node_stream(
                    response,
                    ctx,
                    model.clone(),
                    Arc::clone(&state.billing),
                    request.stream_options,
                );
                Ok(Sse::new(stream).into_response())
            } else {
                // 非流式路径：保持现有逻辑
                let response = node_gateway
                    .enqueue_and_wait(auth.user_id, model.clone(), payload)
                    .await
                    .map_err(ApiError::from)?;

                // 更新 token 计数到 ctx（用于计费）
                ctx.set_input_tokens(response.usage.prompt_tokens);
                ctx.add_output_tokens(response.usage.completion_tokens);

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
                            content: response.choices.first().map(|c| c.message.content.clone()),
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

                // 触发计费
                let billing = Arc::clone(&state.billing);
                let _ = billing
                    .finalize_and_trigger_distribution(
                        &ctx,
                        "node",
                        uuid::Uuid::nil(),
                        "success",
                        auth.user_id,
                    )
                    .await;

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
            let rx = match tokio::time::timeout(
                timeout_duration,
                state.gateway.execute(
                    Arc::clone(&ctx),
                    plan,
                    Arc::clone(&state.account_states),
                    Some(Arc::clone(&state.provider_health)),
                ),
            )
            .await
            {
                Ok(result) => {
                    result.map_err(|e| ApiError::Internal(format!("Execution failed: {}", e)))?
                }
                Err(_) => {
                    tracing::error!(
                        request_id = %request_id.0,
                        timeout_secs = state.gateway_config.timeout_secs,
                        "Gateway execute timeout"
                    );
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
            let billing = Arc::clone(&state.billing);
            let is_stream = request.stream;
            let model = request.model;
            let stream_options = request.stream_options;

            if is_stream {
                // 流式响应
                let stream = create_openai_stream(
                    rx,
                    ctx,
                    model,
                    primary_provider,
                    primary_account_id,
                    billing,
                    stream_options,
                );
                Ok(Sse::new(stream).into_response())
            } else {
                // 非流式响应：收集所有内容后返回完整 JSON
                let response = create_openai_response(
                    rx,
                    ctx,
                    model,
                    primary_provider,
                    primary_account_id,
                    billing,
                )
                .await?;
                Ok(Json(response).into_response())
            }
        }
    }
}

/// 创建 OpenAI 格式的非流式响应
async fn create_openai_response(
    mut rx: tokio::sync::mpsc::Receiver<keycompute_provider_trait::StreamEvent>,
    ctx: Arc<RequestContext>,
    model: String,
    provider_name: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
) -> Result<ChatCompletionResponse> {
    let completion_id = format!(
        "chatcmpl-{}-kc",
        uuid::Uuid::new_v4()
            .to_string()
            .replace("-", "")
            .to_lowercase()
    );
    let created = chrono::Utc::now().timestamp();

    let mut content = String::new();
    let mut finish_reason: Option<String> = None;
    let mut status = "success".to_string();

    // 收集所有内容
    while let Some(event) = rx.recv().await {
        match event {
            keycompute_provider_trait::StreamEvent::Delta {
                content: delta,
                finish_reason: reason,
            } => {
                content.push_str(&delta);
                if reason.is_some() {
                    finish_reason = reason;
                }
            }
            keycompute_provider_trait::StreamEvent::Done => {
                break;
            }
            keycompute_provider_trait::StreamEvent::Error { message } => {
                status = "error".to_string();
                tracing::error!(
                    request_id = %ctx.request_id,
                    error = %message,
                    "Stream error during non-streaming response"
                );
                let _ = billing
                    .finalize_and_trigger_distribution(
                        &ctx,
                        &provider_name,
                        account_id,
                        &status,
                        ctx.user_id,
                    )
                    .await;
                return Err(ApiError::Internal(message));
            }
            _ => {}
        }
    }

    // 执行 billing
    let _ = billing
        .finalize_and_trigger_distribution(&ctx, &provider_name, account_id, &status, ctx.user_id)
        .await;

    // 获取用量信息
    let (prompt_tokens, completion_tokens) = ctx.usage_snapshot();

    Ok(ChatCompletionResponse {
        id: completion_id,
        object: "chat.completion".to_string(),
        created,
        model,
        choices: vec![ChatCompletionChoice {
            index: 0,
            message: ChatCompletionMessage {
                role: "assistant".to_string(),
                content: Some(content),
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
    })
}

/// 创建 OpenAI 格式的 SSE 流
fn create_openai_stream(
    mut rx: tokio::sync::mpsc::Receiver<keycompute_provider_trait::StreamEvent>,
    ctx: Arc<RequestContext>,
    model: String,
    provider_name: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
    stream_options: Option<StreamOptions>,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    async_stream::stream! {
        let mut status = "success".to_string();
        let mut completed = false; // 跟踪流是否正常完成
        let mut first_chunk = true;
        let completion_id = format!("chatcmpl-{}-kc", uuid::Uuid::new_v4().to_string().replace("-", "").to_lowercase());
        let created = chrono::Utc::now().timestamp();

        while let Some(event) = rx.recv().await {
            match event {
                keycompute_provider_trait::StreamEvent::Delta { content, finish_reason } => {
                    let delta = if first_chunk {
                        first_chunk = false;
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
                        id: completion_id.clone(),
                        object: "chat.completion.chunk".to_string(),
                        created,
                        model: model.clone(),
                        system_fingerprint: Some(format!("fp_{}", provider_name)),
                        choices: vec![ChatCompletionChunkChoice {
                            index: 0,
                            delta,
                            finish_reason: finish_reason.clone(),
                            logprobs: None,
                        }],
                        usage: None,
                    };

                    let data = serde_json::to_string(&chunk).unwrap();
                    yield Ok(Event::default().data(data));

                    // 如果有 finish_reason，这是最后一块，发送 [DONE] 并结束
                    if finish_reason.is_some() {
                        completed = true;
                        // 执行 billing
                        let _ = billing.finalize_and_trigger_distribution(
                            &ctx, &provider_name, account_id, &status, ctx.user_id
                        ).await;

                        // 如果需要包含用量信息
                        if stream_options.as_ref().map(|o| o.include_usage).unwrap_or(false) {
                            let (input_tokens, output_tokens) = ctx.usage_snapshot();
                            let usage_chunk = ChatCompletionChunk {
                                id: completion_id.clone(),
                                object: "chat.completion.chunk".to_string(),
                                created,
                                model: model.clone(),
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
                            let data = serde_json::to_string(&usage_chunk).unwrap();
                            yield Ok(Event::default().data(data));
                        }

                        // 发送 [DONE] 标记
                        yield Ok(Event::default().data("[DONE]"));
                        break;
                    }
                }
                keycompute_provider_trait::StreamEvent::Done => {
                    // 流正常结束
                    completed = true;
                    let _ = billing.finalize_and_trigger_distribution(
                        &ctx, &provider_name, account_id, &status, ctx.user_id
                    ).await;

                    // 如果需要包含用量信息
                    if stream_options.as_ref().map(|o| o.include_usage).unwrap_or(false) {
                        let (input_tokens, output_tokens) = ctx.usage_snapshot();
                        let usage_chunk = ChatCompletionChunk {
                            id: completion_id.clone(),
                            object: "chat.completion.chunk".to_string(),
                            created,
                            model: model.clone(),
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
                        let data = serde_json::to_string(&usage_chunk).unwrap();
                        yield Ok(Event::default().data(data));
                    }

                    yield Ok(Event::default().data("[DONE]"));
                    break;
                }
                keycompute_provider_trait::StreamEvent::Error { message } => {
                    completed = true;
                    status = "error".to_string();
                    let _ = billing.finalize_and_trigger_distribution(
                        &ctx, &provider_name, account_id, &status, ctx.user_id
                    ).await;

                    let error_chunk = serde_json::json!({
                        "error": {
                            "message": message,
                            "type": "api_error",
                            "code": "internal_error"
                        }
                    });
                    yield Ok(Event::default().data(error_chunk.to_string()));
                    break;
                }
                _ => {}
            }
        }

        // 流意外结束（channel 关闭但没有收到完成事件）
        if !completed {
            tracing::warn!(
                request_id = %ctx.request_id,
                "Stream ended without Done/Error/finish_reason event"
            );
            status = "incomplete".to_string();
            let _ = billing.finalize_and_trigger_distribution(
                &ctx, &provider_name, account_id, &status, ctx.user_id
            ).await;
        }
    }
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

/// 列出所有模型
/// GET /v1/models
/// 从数据库聚合所有启用的 Provider 账号支持的模型列表
pub async fn list_models(State(state): State<AppState>) -> Result<Json<ListModelsResponse>> {
    let mut model_set = std::collections::HashSet::new();
    let mut provider_map = std::collections::HashMap::new();

    // 尝试从数据库获取模型列表
    if let Some(pool) = state.pool.as_ref() {
        // 查询所有启用的账号（不限制 tenant_id，使用系统级查询）
        if let Ok(accounts) = Account::find_enabled_all(pool).await {
            for account in accounts {
                for model in account.models_supported {
                    model_set.insert(model.clone());
                    provider_map.insert(model, account.provider.clone());
                }
            }
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
        .as_ref()
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
    billing: Arc<keycompute_billing::BillingService>,
    stream_options: Option<StreamOptions>,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    async_stream::stream! {
        let completion_id = format!("chatcmpl-{}-kc", uuid::Uuid::new_v4().to_string().replace("-", "").to_lowercase());
        let created = chrono::Utc::now().timestamp();

        // 获取响应内容
        let content = response.choices.first().map(|c| c.message.content.clone()).unwrap_or_default();

        // 将内容拆分为字符级别的 chunk（模拟 token 级输出）
        // 注：这里是简单实现，按字符拆分，实际可以按 token 拆分
        let chars: Vec<char> = content.chars().collect();
        let chunk_size = std::cmp::max(1, chars.len() / 20); // 至少 1 个字符，最多 20 个 chunk

        // 发送 content chunks
        for chunk in chars.chunks(chunk_size) {
            let chunk_content: String = chunk.iter().collect();
            let data = serde_json::json!({
                "id": completion_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {
                        "role": "assistant",
                        "content": chunk_content
                    },
                    "finish_reason": null
                }]
            });
            yield Ok(Event::default().event("message").data(data.to_string()));

            // 小延迟，模拟真实流式输出
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        // 发送最后一个带有 finish_reason 的 chunk
        let finish_reason = response.choices.first().and_then(|c| c.finish_reason.clone()).unwrap_or("stop".to_string());
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
        yield Ok(Event::default().event("message").data(data.to_string()));

        // 如果请求了 usage，发送 usage chunk
        if stream_options.map(|o| o.include_usage).unwrap_or(false) {
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
            yield Ok(Event::default().event("message").data(data.to_string()));
        }

        // 计费（与 Provider 路径一致）
        let _ = billing
            .finalize_and_trigger_distribution(
                &ctx,
                "node",
                uuid::Uuid::nil(),
                "success",
                ctx.user_id,
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_completion_request_deserialize() {
        let json = r#"{
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello"}],
            "temperature": 0.7,
            "max_tokens": 100
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
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
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert!(req.stream);
        assert!(req.stream_options.unwrap().include_usage);
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
}
