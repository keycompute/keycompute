//! Gateway 执行器
//!
//! 核心执行入口，控制 retry/fallback/streaming 生命周期。

use crate::{GatewayConfig, streaming::StreamPipeline};
use keycompute_runtime::AccountStateStore;
use keycompute_types::{ExecutionPlan, ExecutionTarget, KeyComputeError, RequestContext, Result};
use keycompute_provider_trait::{ProviderAdapter, StreamEvent, UpstreamRequest};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use futures::StreamExt;

/// Gateway 执行器
///
/// 唯一执行层，负责：
/// 1. 执行请求到上游 Provider
/// 2. 处理 retry 和 fallback
/// 3. 管理 streaming 生命周期
/// 4. 更新运行时状态
#[derive(Debug)]
pub struct GatewayExecutor {
    config: GatewayConfig,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
}

impl GatewayExecutor {
    /// 创建新的执行器
    pub fn new(config: GatewayConfig, providers: HashMap<String, Arc<dyn ProviderAdapter>>) -> Self {
        Self {
            config,
            providers,
        }
    }

    /// 执行请求（唯一执行入口）
    ///
    /// 执行流程：
    /// 1. 尝试 primary target
    /// 2. 失败则 fallback 到下一个 target
    /// 3. 成功后更新账号状态
    pub async fn execute(
        &self,
        ctx: Arc<RequestContext>,
        plan: ExecutionPlan,
        account_states: Arc<AccountStateStore>,
    ) -> Result<mpsc::Receiver<StreamEvent>> {
        let (tx, rx) = mpsc::channel(100);

        // 构建 target 链：primary + fallback
        let mut targets = vec![plan.primary];
        targets.extend(plan.fallback_chain);

        let mut last_error = None;

        for target in targets {
            match self.try_execute(&ctx, &target, tx.clone()).await {
                Ok(()) => {
                    // 成功：标记账号状态
                    account_states.mark_success(target.account_id);
                    tracing::info!(
                        request_id = %ctx.request_id,
                        provider = %target.provider,
                        "Request executed successfully"
                    );
                    return Ok(rx);
                }
                Err(e) => {
                    // 失败：标记错误，继续 fallback
                    account_states.mark_error(target.account_id);
                    tracing::warn!(
                        request_id = %ctx.request_id,
                        provider = %target.provider,
                        error = %e,
                        "Request failed, trying fallback"
                    );
                    last_error = Some(e);
                }
            }
        }

        // 所有 target 都失败
        Err(last_error.unwrap_or(KeyComputeError::RoutingFailed))
    }

    /// 尝试执行单个 target
    async fn try_execute(
        &self,
        ctx: &RequestContext,
        target: &ExecutionTarget,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<()> {
        // 获取 Provider
        let provider = self.providers.get(&target.provider)
            .ok_or_else(|| KeyComputeError::Internal(format!("Provider {} not found", target.provider)))?;

        // 构建上游请求
        let request = self.build_upstream_request(ctx, target);

        // 执行流式请求
        let mut stream = provider.stream_chat(request).await?;

        // 流处理管道
        let mut pipeline = StreamPipeline::new(ctx.request_id);

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::Delta { content, finish_reason } => {
                    // 累积 tokens（简化估算）
                    let tokens = Self::estimate_tokens(&content);
                    ctx.usage.add_output(tokens);

                    // 转发给客户端
                    let event = StreamEvent::Delta { content, finish_reason };
                    pipeline.process_event(&event);
                    tx.send(event).await
                        .map_err(|_| KeyComputeError::Internal("Send error".into()))?;
                }
                StreamEvent::Usage { input_tokens, output_tokens } => {
                    // Provider 报告的用量（优先级更高）
                    ctx.usage.set_input(input_tokens);
                    // 覆盖输出的 token 计数
                    let current_output = ctx.usage.snapshot().1;
                    if output_tokens > current_output {
                        ctx.usage.add_output(output_tokens - current_output);
                    }
                }
                StreamEvent::Done => {
                    tx.send(StreamEvent::Done).await
                        .map_err(|_| KeyComputeError::Internal("Send error".into()))?;
                    break;
                }
                StreamEvent::Error { message } => {
                    return Err(KeyComputeError::ProviderError(message));
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// 构建上游请求
    fn build_upstream_request(&self, ctx: &RequestContext, target: &ExecutionTarget) -> UpstreamRequest {
        let messages: Vec<keycompute_provider_trait::UpstreamMessage> = ctx.messages
            .iter()
            .map(|m| keycompute_provider_trait::UpstreamMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        UpstreamRequest {
            endpoint: target.endpoint.clone(),
            api_key: target.api_key.clone(),
            model: ctx.model.clone(),
            messages,
            stream: ctx.stream,
            max_tokens: None,
            temperature: None,
            top_p: None,
        }
    }

    /// 估算 token 数（简化实现）
    fn estimate_tokens(content: &str) -> u32 {
        // 粗略估算：1 token ≈ 4 个字符
        ((content.len() / 4) as u32).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keycompute_types::{Message, PricingSnapshot, UsageAccumulator};
    use rust_decimal::Decimal;

    fn create_test_context() -> RequestContext {
        RequestContext {
            request_id: uuid::Uuid::new_v4(),
            user_id: uuid::Uuid::new_v4(),
            tenant_id: uuid::Uuid::new_v4(),
            api_key_id: uuid::Uuid::new_v4(),
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
            stream: true,
            pricing_snapshot: PricingSnapshot {
                model_name: "gpt-4o".to_string(),
                currency: "CNY".to_string(),
                input_price_per_1k: Decimal::from(1),
                output_price_per_1k: Decimal::from(2),
            },
            usage: UsageAccumulator::default(),
            started_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_gateway_executor_new() {
        let config = GatewayConfig::default();
        let providers = HashMap::new();
        let executor = GatewayExecutor::new(config, providers);
        assert_eq!(executor.config.max_retries, 3);
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(GatewayExecutor::estimate_tokens("Hello"), 1);
        assert_eq!(GatewayExecutor::estimate_tokens("a".repeat(100).as_str()), 25);
    }
}
