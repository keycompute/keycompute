//! Anthropic Messages API 兼容处理器
//!
//! 提供与 Anthropic Messages API 完全兼容的接口
//! 底层可以路由到任意 provider (DeepSeek, Doubao, OpenAI, Claude 等)
//!
//! 参考: https://docs.anthropic.com/claude/reference/messages_post

use crate::{
    error::{ApiError, Result},
    extractors::{AuthExtractor, RequestId},
    state::AppState,
};
use axum::{
    Json,
    extract::State,
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
};
use futures::stream::Stream;
use keycompute_types::{Message, MessageRole, RequestContext};
use std::convert::Infallible;
use std::sync::Arc;

// 复用 keycompute-claude 的协议定义
use keycompute_claude::protocol::{
    ClaudeRequest, ClaudeResponse, ClaudeContent,
    ClaudeUsage, ContentBlock,
};

/// POST /v1/messages
///
/// 接收 Anthropic 格式请求，底层路由到任意 provider，返回 Anthropic 格式响应
pub async fn create_message(
    State(state): State<AppState>,
    auth: AuthExtractor,
    request_id: RequestId,
    Json(request): Json<ClaudeRequest>,
) -> Result<axum::response::Response> {
    // 1. 余额预检查
    if let Some(balance_service) = state.billing.balance_service() {
        balance_service
            .check_balance_for_tenant(auth.user_id, auth.tenant_id)
            .await
            .map_err(ApiError::from)?;
    }

    // 2. 构建定价快照
    let pricing = state
        .pricing
        .create_snapshot(&request.model, &auth.tenant_id, None)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create pricing snapshot: {}", e)))?;

    // 3. 转换消息格式（Anthropic -> 内部格式）
    let mut messages = Vec::new();

    // system 消息单独处理（Anthropic 的 system 是独立字段）
    if let Some(system) = &request.system {
        messages.push(Message {
            role: MessageRole::System,
            content: system.clone(),
        });
    }

    // 转换普通消息
    for msg in &request.messages {
        let role = match msg.role.as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            _ => MessageRole::User, // 默认为 user
        };

        // 提取内容（支持纯文本和 block 数组）
        let content = match &msg.content {
            ClaudeContent::Text(text) => text.clone(),
            ClaudeContent::Blocks(blocks) => {
                // 只提取文本块，其他类型暂不支持
                blocks.iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None, // image 等暂不支持
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };

        messages.push(Message { role, content });
    }

    // 4. 构建 RequestContext
    let mut ctx = Arc::new(RequestContext::new(
        auth.user_id,
        auth.tenant_id,
        auth.produce_ai_key_id,
        request.model.clone(),
        messages,
        request.stream.unwrap_or(false),
        pricing,
    ));

    // 5. 智能路由
    let plan = state
        .routing
        .route(&ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("Routing failed: {}", e)))?;

    let primary_provider = plan.primary.provider.clone();
    let primary_account_id = plan.primary.account_id;

    // 6. 更新定价（根据实际 provider）
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
        stream = %request.stream.unwrap_or(false),
        primary_provider = %primary_provider,
        "Anthropic message request"
    );

    // 7. 执行（带超时）
    let timeout_duration = std::time::Duration::from_secs(state.gateway_config.timeout_secs);
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
        Ok(result) => result.map_err(|e| ApiError::Internal(format!("Execution failed: {}", e)))?,
        Err(_) => {
            return Err(ApiError::Internal(format!(
                "Gateway execute timeout after {}s",
                state.gateway_config.timeout_secs
            )));
        }
    };

    // 8. 根据 stream 参数返回流式或非流式响应
    let billing = Arc::clone(&state.billing);
    let is_stream = request.stream.unwrap_or(false);
    let model = request.model;

    if is_stream {
        let stream = create_anthropic_stream(
            rx, ctx, model, primary_provider, primary_account_id, billing,
        );
        Ok(Sse::new(stream).into_response())
    } else {
        let response = create_anthropic_response(
            rx, ctx, model, primary_provider, primary_account_id, billing,
        )
        .await?;
        Ok(Json(response).into_response())
    }
}

/// 创建 Anthropic 格式的 SSE 流
fn create_anthropic_stream(
    mut rx: tokio::sync::mpsc::Receiver<keycompute_provider_trait::StreamEvent>,
    ctx: Arc<RequestContext>,
    model: String,
    provider_name: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    async_stream::stream! {
        let message_id = format!("msg_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
        let mut status = "success".to_string();
        let mut completed = false;
        let mut total_input_tokens: u32 = 0;
        let mut total_output_tokens: u32 = 0;
        let mut has_sent_message_start = false;
        let mut has_sent_content_start = false;
        let mut stop_reason: Option<String> = None;

        /// 内联发送 message_start 和 content_block_start
        macro_rules! send_start_events {
            ($input_tokens:expr) => {
                if !has_sent_message_start {
                    has_sent_message_start = true;

                    let msg_start = serde_json::json!({
                        "type": "message_start",
                        "message": {
                            "id": message_id,
                            "type": "message",
                            "role": "assistant",
                            "content": [],
                            "model": model,
                            "stop_reason": serde_json::Value::Null,
                            "stop_sequence": serde_json::Value::Null,
                            "usage": {
                                "input_tokens": $input_tokens,
                                "output_tokens": 0
                            }
                        }
                    });
                    yield Ok(Event::default().event("message_start").data(msg_start.to_string()));

                    // content_block_start
                    has_sent_content_start = true;
                    let cb_start = serde_json::json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    });
                    yield Ok(Event::default().event("content_block_start").data(cb_start.to_string()));
                }
            };
        }

        while let Some(event) = rx.recv().await {
            match event {
                keycompute_provider_trait::StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    if input_tokens > 0 {
                        total_input_tokens = input_tokens;
                    }
                    if output_tokens > 0 {
                        total_output_tokens = output_tokens;
                    }

                    send_start_events!(total_input_tokens);
                }
                keycompute_provider_trait::StreamEvent::Delta {
                    content,
                    finish_reason,
                } => {
                    send_start_events!(total_input_tokens);

                    if !content.is_empty() {
                        let delta = serde_json::json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {
                                "type": "text_delta",
                                "text": content
                            }
                        });
                        yield Ok(Event::default().event("content_block_delta").data(delta.to_string()));
                    }

                    if let Some(reason) = finish_reason {
                        stop_reason = Some(reason);
                    }
                }
                keycompute_provider_trait::StreamEvent::Done => {
                    // 发送 content_block_stop
                    if has_sent_content_start {
                        let cb_stop = serde_json::json!({
                            "type": "content_block_stop",
                            "index": 0
                        });
                        yield Ok(Event::default().event("content_block_stop").data(cb_stop.to_string()));
                    }

                    // 映射 finish_reason: OpenAI 风格 -> Anthropic 风格
                    let anthropic_stop_reason = stop_reason.as_deref().map(|r| match r {
                        "stop" => "end_turn",
                        "length" => "max_tokens",
                        "tool_calls" => "tool_use",
                        "max_tokens" => "max_tokens",
                        "end_turn" => "end_turn",
                        _ => "end_turn",
                    });

                    // 发送 message_delta
                    let msg_delta = serde_json::json!({
                        "type": "message_delta",
                        "delta": {
                            "stop_reason": anthropic_stop_reason,
                            "stop_sequence": serde_json::Value::Null
                        },
                        "usage": {
                            "output_tokens": total_output_tokens
                        }
                    });
                    yield Ok(Event::default().event("message_delta").data(msg_delta.to_string()));

                    // 发送 message_stop
                    yield Ok(Event::default().event("message_stop").data(r#"{"type":"message_stop"}"#));

                    completed = true;
                    let _ = billing
                        .finalize_and_trigger_distribution(&ctx, &provider_name, account_id, &status, ctx.user_id)
                        .await;
                    break;
                }
                keycompute_provider_trait::StreamEvent::Error { message } => {
                    completed = true;
                    status = "error".to_string();
                    tracing::error!(
                        request_id = %ctx.request_id,
                        error = %message,
                        "Stream error during Anthropic streaming response"
                    );
                    let _ = billing
                        .finalize_and_trigger_distribution(&ctx, &provider_name, account_id, &status, ctx.user_id)
                        .await;

                    let error_data = serde_json::json!({
                        "type": "error",
                        "error": {
                            "type": "api_error",
                            "message": message
                        }
                    });
                    yield Ok(Event::default().event("error").data(error_data.to_string()));
                    break;
                }
                _ => {}
            }
        }

        if !completed {
            tracing::warn!(
                request_id = %ctx.request_id,
                "Anthropic stream ended without Done/Error/finish_reason event"
            );
            status = "incomplete".to_string();
            let _ = billing
                .finalize_and_trigger_distribution(&ctx, &provider_name, account_id, &status, ctx.user_id)
                .await;
        }
    }
}
async fn create_anthropic_response(
    mut rx: tokio::sync::mpsc::Receiver<keycompute_provider_trait::StreamEvent>,
    ctx: Arc<RequestContext>,
    model: String,
    provider_name: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
) -> Result<ClaudeResponse> {
    let message_id = format!("msg_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
    let mut content = String::new();
    let mut stop_reason: Option<String> = None;
    let mut status = "success".to_string();

    // 收集所有内容
    while let Some(event) = rx.recv().await {
        match event {
            keycompute_provider_trait::StreamEvent::Delta {
                content: delta,
                finish_reason: reason,
            } => {
                content.push_str(&delta);
                if let Some(r) = reason {
                    // 映射 finish_reason: OpenAI 风格 -> Anthropic 风格
                    stop_reason = Some(match r.as_str() {
                        "stop" => "end_turn",
                        "length" => "max_tokens",
                        "tool_calls" => "tool_use",
                        _ => "end_turn",
                    }.to_string());
                }
            }
            keycompute_provider_trait::StreamEvent::Done => break,
            keycompute_provider_trait::StreamEvent::Error { message } => {
                status = "error".to_string();
                tracing::error!(
                    request_id = %ctx.request_id,
                    error = %message,
                    "Stream error during Anthropic response"
                );
                let _ = billing
                    .finalize_and_trigger_distribution(&ctx, &provider_name, account_id, &status, ctx.user_id)
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

    // 获取用量
    let (input_tokens, output_tokens) = ctx.usage_snapshot();

    Ok(ClaudeResponse {
        id: message_id,
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        model,
        stop_reason,
        stop_sequence: None,
        content: vec![ContentBlock::Text { text: content }],
        usage: ClaudeUsage {
            input_tokens,
            output_tokens,
        },
    })
}
