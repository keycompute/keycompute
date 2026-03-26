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
    response::sse::{Event, Sse},
};
use futures::stream::Stream;
use keycompute_db::models::account::Account;
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey};
use keycompute_types::{Message, RequestContext, UsageAccumulator};
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
pub async fn chat_completions(
    State(state): State<AppState>,
    auth: AuthExtractor,
    request_id: RequestId,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>> {
    // 1. 配额控制 - 创建限流键并检查
    let rate_limit_key = RateLimitKey::new(auth.tenant_id, auth.user_id, auth.produce_ai_key_id);

    // 尝试从数据库加载租户配额配置
    let rate_limit_config = if let Some(pool) = &state.pool {
        match keycompute_db::Tenant::find_by_id(pool, auth.tenant_id).await {
            Ok(Some(tenant)) => {
                RateLimitConfig::from_tenant(tenant.default_rpm_limit, tenant.default_tpm_limit)
            }
            Ok(None) => {
                tracing::warn!(
                    tenant_id = %auth.tenant_id,
                    "Tenant not found for rate limiting, using default config"
                );
                RateLimitConfig::default()
            }
            Err(e) => {
                tracing::warn!(
                    tenant_id = %auth.tenant_id,
                    error = %e,
                    "Failed to load tenant for rate limiting, using default config"
                );
                RateLimitConfig::default()
            }
        }
    } else {
        RateLimitConfig::default()
    };

    // 检查并记录请求配额
    state
        .rate_limiter
        .check_and_record_with_config(&rate_limit_key, &rate_limit_config)
        .await
        .map_err(|e| {
            tracing::warn!(
                tenant_id = %auth.tenant_id,
                user_id = %auth.user_id,
                rpm_limit = rate_limit_config.rpm_limit,
                error = %e,
                "Rate limit exceeded"
            );
            ApiError::RateLimit(format!(
                "Rate limit exceeded. RPM limit: {}",
                rate_limit_config.rpm_limit
            ))
        })?;

    tracing::debug!(
        tenant_id = %auth.tenant_id,
        user_id = %auth.user_id,
        rpm_limit = rate_limit_config.rpm_limit,
        tpm_limit = rate_limit_config.tpm_limit,
        "Rate limit check passed"
    );

    // 2. 构建 PricingSnapshot
    let pricing = state
        .pricing
        .create_snapshot(&request.model, &auth.tenant_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create pricing snapshot: {}", e)))?;

    // 3. 转换消息格式
    let messages: Vec<Message> = request
        .messages
        .iter()
        .map(|m| Message {
            role: m.role.clone(),
            content: m.content.clone().unwrap_or_default(),
        })
        .collect();

    // 4. 构建 RequestContext
    let ctx = Arc::new(RequestContext {
        request_id: request_id.0,
        user_id: auth.user_id,
        tenant_id: auth.tenant_id,
        produce_ai_key_id: auth.produce_ai_key_id,
        model: request.model.clone(),
        messages,
        stream: request.stream,
        pricing_snapshot: pricing,
        usage: UsageAccumulator::default(),
        started_at: chrono::Utc::now(),
    });

    // 5. 智能路由
    let plan = state
        .routing
        .route(&ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("Routing failed: {}", e)))?;

    let primary_provider = plan.primary.provider.clone();
    let primary_account_id = plan.primary.account_id;

    tracing::info!(
        request_id = %request_id.0,
        model = %request.model,
        stream = %request.stream,
        primary_provider = %primary_provider,
        "Chat completion request"
    );

    // 5. 执行
    let rx = state
        .gateway
        .execute(
            Arc::clone(&ctx),
            plan,
            Arc::clone(&state.account_states),
            Some(Arc::clone(&state.provider_health)),
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Execution failed: {}", e)))?;

    // 6. 返回 SSE 流
    let billing = Arc::clone(&state.billing);
    let stream = create_openai_stream(
        rx,
        ctx,
        request.model,
        primary_provider,
        primary_account_id,
        billing,
        request.stream_options,
    );

    Ok(Sse::new(stream))
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
    use futures::stream::StreamExt;

    async_stream::stream! {
        let mut status = "success".to_string();
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

                    // 如果有 finish_reason，这是最后一块
                    if finish_reason.is_some() {
                        break;
                    }
                }
                keycompute_provider_trait::StreamEvent::Done => {
                    // 流正常结束
                    let _ = billing.finalize_and_trigger_distribution(
                        &ctx, &provider_name, account_id, &status, ctx.user_id
                    ).await;

                    // 如果需要包含用量信息
                    if stream_options.as_ref().map(|o| o.include_usage).unwrap_or(false) {
                        let (input_tokens, output_tokens) = ctx.usage.snapshot();
                        let usage_chunk = ChatCompletionChunk {
                            id: completion_id.clone(),
                            object: "chat.completion.chunk".to_string(),
                            created,
                            model: model.clone(),
                            system_fingerprint: Some(format!("fp_{}", provider_name)),
                            choices: vec![],
                            usage: Some(CompletionUsage {
                                prompt_tokens: input_tokens as u32,
                                completion_tokens: output_tokens as u32,
                                total_tokens: (input_tokens + output_tokens) as u32,
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

        // 流意外结束
        if status == "success" {
            tracing::warn!(
                request_id = %ctx.request_id,
                "Stream ended without Done/Error event"
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

    // 如果数据库中没有模型，使用默认模型列表（向后兼容）
    if model_set.is_empty() {
        model_set.insert("gpt-4o".to_string());
        model_set.insert("gpt-4o-mini".to_string());
        model_set.insert("gpt-4-turbo".to_string());
        model_set.insert("gpt-3.5-turbo".to_string());
        model_set.insert("claude-3-5-sonnet-20241022".to_string());
        model_set.insert("deepseek-chat".to_string());

        provider_map.insert("gpt-4o".to_string(), "openai".to_string());
        provider_map.insert("gpt-4o-mini".to_string(), "openai".to_string());
        provider_map.insert("gpt-4-turbo".to_string(), "openai".to_string());
        provider_map.insert("gpt-3.5-turbo".to_string(), "openai".to_string());
        provider_map.insert(
            "claude-3-5-sonnet-20241022".to_string(),
            "anthropic".to_string(),
        );
        provider_map.insert("deepseek-chat".to_string(), "deepseek".to_string());
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
        assert_eq!(req.stream_options.unwrap().include_usage, true);
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
