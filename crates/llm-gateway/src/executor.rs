//! Gateway 执行器
//!
//! 核心执行入口，控制 retry/fallback/streaming 生命周期。
//!
//! 健康状态集成：
//! - 执行成功时调用 ProviderHealthStore::record_success
//! - 执行失败时调用 ProviderHealthStore::record_failure
//! - 延迟时间从请求开始计算
//!
//! Internal HTTP Proxy 集成：
//! - 统一上游连接管理
//! - 多代理出口支持
//! - 请求追踪

use crate::{GatewayConfig, HttpProxy, streaming::StreamPipeline};
use futures::StreamExt;
use keycompute_provider_trait::{HttpTransport, ProviderAdapter, StreamEvent, UpstreamRequest};
use keycompute_routing::{AccountStateStore, ProviderHealthStore};
use keycompute_types::{ExecutionPlan, ExecutionTarget, KeyComputeError, RequestContext, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Gateway 执行器
///
/// 唯一执行层，负责：
/// 1. 执行请求到上游 Provider
/// 2. 处理 retry 和 fallback
/// 3. 管理 streaming 生命周期
/// 4. 更新运行时状态（账号状态 + Provider 健康状态）
///
/// Internal HTTP Proxy 集成：
/// - 统一连接池管理
/// - 多代理出口支持
/// - 请求追踪
#[derive(Debug)]
pub struct GatewayExecutor {
    #[allow(dead_code)]
    config: GatewayConfig,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    /// Internal HTTP Proxy（统一上游连接管理）
    http_proxy: Option<Arc<HttpProxy>>,
}

impl GatewayExecutor {
    /// 创建新的执行器
    pub fn new(
        config: GatewayConfig,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
    ) -> Self {
        Self {
            config,
            providers,
            http_proxy: None,
        }
    }

    /// 创建带 HTTP Proxy 的执行器
    pub fn with_proxy(
        config: GatewayConfig,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        http_proxy: Arc<HttpProxy>,
    ) -> Self {
        Self {
            config,
            providers,
            http_proxy: Some(http_proxy),
        }
    }

    /// 获取 HTTP Proxy
    pub fn http_proxy(&self) -> Option<&Arc<HttpProxy>> {
        self.http_proxy.as_ref()
    }

    /// 设置 HTTP Proxy
    pub fn set_http_proxy(&mut self, proxy: Arc<HttpProxy>) {
        self.http_proxy = Some(proxy);
    }

    /// 执行请求（唯一执行入口）
    ///
    /// 执行流程：
    /// 1. 尝试 primary target
    /// 2. 失败则 fallback 到下一个 target
    /// 3. 成功后更新账号状态和 Provider 健康状态
    ///
    /// # 参数
    /// - `ctx`: 请求上下文
    /// - `plan`: 执行计划（包含 primary 和 fallback chain）
    /// - `account_states`: 账号状态存储
    /// - `provider_health`: Provider 健康状态存储（可选，用于被动记录健康状态）
    pub async fn execute(
        &self,
        ctx: Arc<RequestContext>,
        plan: ExecutionPlan,
        account_states: Arc<AccountStateStore>,
        provider_health: Option<Arc<ProviderHealthStore>>,
    ) -> Result<mpsc::Receiver<StreamEvent>> {
        let (tx, rx) = mpsc::channel(100);

        // 在后台任务中实际执行上游请求，避免在返回 rx 之前就被有界 channel 背压阻塞。
        // 这对流式场景尤其重要：handler 需要先拿到 rx，才能开始消费事件并向客户端推送。
        let runner = Self {
            config: self.config.clone(),
            providers: self.providers.clone(),
            http_proxy: self.http_proxy.clone(),
        };

        tokio::spawn(async move {
            if let Err(error) = runner
                .run_plan(
                    Arc::clone(&ctx),
                    plan,
                    tx.clone(),
                    account_states,
                    provider_health,
                )
                .await
            {
                tracing::error!(
                    request_id = %ctx.request_id,
                    error = %error,
                    "Execution task failed"
                );
                let _ = tx.send(StreamEvent::error(error.to_string())).await;
            }
        });

        Ok(rx)
    }

    async fn run_plan(
        &self,
        ctx: Arc<RequestContext>,
        plan: ExecutionPlan,
        tx: mpsc::Sender<StreamEvent>,
        account_states: Arc<AccountStateStore>,
        provider_health: Option<Arc<ProviderHealthStore>>,
    ) -> Result<()> {
        // 构建 target 链：primary + fallback
        let mut targets = vec![plan.primary];
        targets.extend(plan.fallback_chain);

        let mut last_error = None;
        let _start_time = Instant::now();
        let mut is_primary = true;

        for target in targets {
            let target_start = Instant::now();
            match self.try_execute(&ctx, &target, tx.clone()).await {
                Ok(()) => {
                    // 成功：标记账号状态
                    account_states.mark_success(target.account_id);

                    // 成功：更新 Provider 健康状态
                    let latency_ms = target_start.elapsed().as_millis() as u64;
                    if let Some(ref health_store) = provider_health {
                        health_store.record_success(&target.provider, latency_ms);
                        // 如果不是 primary，说明使用了 fallback
                        if !is_primary {
                            health_store.record_fallback();
                        }
                    }

                    tracing::info!(
                        request_id = %ctx.request_id,
                        provider = %target.provider,
                        latency_ms = latency_ms,
                        is_fallback = !is_primary,
                        "Request executed successfully"
                    );
                    return Ok(());
                }
                Err(e) => {
                    // 注意：不再自动标记错误，错误计数只能通过管理员手动测试 API 触发
                    // 保留 Provider 健康状态更新用于路由评分
                    if let Some(ref health_store) = provider_health {
                        health_store.record_failure(&target.provider);
                    }

                    tracing::warn!(
                        request_id = %ctx.request_id,
                        provider = %target.provider,
                        error = %e,
                        "Request failed, trying fallback"
                    );
                    last_error = Some(e);
                }
            }
            // 第一次循环后，后续都是 fallback
            is_primary = false;
        }

        // 所有 target 都失败
        Err(last_error.unwrap_or_else(|| KeyComputeError::RoutingFailed(ctx.model.clone())))
    }

    /// 尝试执行单个 target
    async fn try_execute(
        &self,
        ctx: &RequestContext,
        target: &ExecutionTarget,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<()> {
        tracing::info!(
            request_id = %ctx.request_id,
            provider = %target.provider,
            endpoint = %target.endpoint,
            "try_execute: starting"
        );

        // 获取 Provider
        let provider = self.providers.get(&target.provider).ok_or_else(|| {
            KeyComputeError::Internal(format!("Provider {} not found", target.provider))
        })?;

        // 获取 HTTP 传输层（优先使用 HttpProxy 中的客户端）
        let transport: Arc<dyn HttpTransport> = if let Some(ref proxy) = self.http_proxy {
            Arc::clone(proxy.default_client()) as Arc<dyn HttpTransport>
        } else {
            // 使用默认传输
            Arc::new(keycompute_provider_trait::DefaultHttpTransport::new())
        };

        // 构建上游请求
        let request = self.build_upstream_request(ctx, target);

        tracing::info!(
            request_id = %ctx.request_id,
            provider = %target.provider,
            "try_execute: calling provider.stream_chat"
        );

        // 执行流式请求（传入 transport）
        let mut stream = provider.stream_chat(transport.as_ref(), request).await?;

        tracing::info!(
            request_id = %ctx.request_id,
            provider = %target.provider,
            "try_execute: stream started, processing events"
        );

        // 流处理管道
        let mut pipeline = StreamPipeline::new(ctx.request_id);

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::Delta {
                    content,
                    finish_reason,
                } => {
                    // 累积 tokens（简化估算）
                    let tokens = Self::estimate_tokens(&content);
                    ctx.add_output_tokens(tokens);

                    // 转发给客户端
                    let event = StreamEvent::Delta {
                        content,
                        finish_reason: finish_reason.clone(),
                    };
                    pipeline.process_event(&event);
                    tx.send(event)
                        .await
                        .map_err(|_| KeyComputeError::Internal("Send error".into()))?;

                    // 如果有 finish_reason，发送 Done 并退出
                    if finish_reason.is_some() {
                        tracing::debug!(
                            request_id = %ctx.request_id,
                            finish_reason = ?finish_reason,
                            "try_execute: received finish_reason, sending Done and exiting"
                        );
                        // 注意：不发送 Done 事件，让 handler 根据 finish_reason 结束
                        break;
                    }
                }
                StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    // Provider 报告的用量（优先级更高）
                    ctx.set_input_tokens(input_tokens);
                    // 覆盖输出的 token 计数
                    let current_output = ctx.usage_snapshot().1;
                    if output_tokens > current_output {
                        ctx.add_output_tokens(output_tokens - current_output);
                    }
                }
                StreamEvent::Done => {
                    tracing::debug!(
                        request_id = %ctx.request_id,
                        "try_execute: received Done event"
                    );
                    tx.send(StreamEvent::Done)
                        .await
                        .map_err(|_| KeyComputeError::Internal("Send error".into()))?;
                    break;
                }
                StreamEvent::Error { message } => {
                    tracing::error!(
                        request_id = %ctx.request_id,
                        message = %message,
                        "try_execute: received Error event"
                    );
                    return Err(KeyComputeError::ProviderError(message));
                }
                _ => {}
            }
        }

        tracing::debug!(
            request_id = %ctx.request_id,
            provider = %target.provider,
            "try_execute: completed successfully"
        );

        Ok(())
    }

    /// 构建上游请求
    fn build_upstream_request(
        &self,
        ctx: &RequestContext,
        target: &ExecutionTarget,
    ) -> UpstreamRequest {
        let messages: Vec<keycompute_provider_trait::UpstreamMessage> = ctx
            .messages
            .iter()
            .map(|m| keycompute_provider_trait::UpstreamMessage {
                role: m.role.to_string(),
                content: m.content.clone(),
            })
            .collect();

        UpstreamRequest {
            endpoint: target.endpoint.clone(),
            upstream_api_key: target.upstream_api_key.clone(),
            model: ctx.model.clone(),
            messages,
            stream: ctx.stream,
            max_tokens: None,
            temperature: None,
            top_p: None,
        }
    }

    /// 精确计算 token 数
    ///
    /// 使用 tiktoken-rs 库的 o200k_base tokenizer（支持 GPT-4o, o1, o3 等模型）
    /// 提供与 OpenAI API 完全一致的 token 计数
    fn estimate_tokens(content: &str) -> u32 {
        if content.is_empty() {
            return 0;
        }

        // 使用 o200k_base tokenizer (GPT-4o, o1, o3 等模型)
        // singleton 模式避免重复加载词表
        let bpe = tiktoken_rs::o200k_base_singleton();
        bpe.encode_with_special_tokens(content).len() as u32
    }

    /// 获取所有 Provider 名称列表
    pub fn list_providers(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// 检查是否存在指定的 Provider
    pub fn has_provider(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    /// 获取 Provider 数量
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// 获取指定 Provider 支持的模型列表
    pub fn get_provider_models(&self, provider_name: &str) -> Vec<String> {
        self.providers
            .get(provider_name)
            .map(|p| {
                p.supported_models()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use keycompute_types::{Message, PricingSnapshot};
    use rust_decimal::Decimal;
    use std::time::Duration;
    use uuid::Uuid;

    #[derive(Debug)]
    struct ManyChunksProvider {
        chunks: usize,
    }

    #[async_trait]
    impl ProviderAdapter for ManyChunksProvider {
        fn name(&self) -> &'static str {
            "many-chunks"
        }

        fn supported_models(&self) -> Vec<&'static str> {
            vec!["gpt-4o"]
        }

        async fn stream_chat(
            &self,
            _transport: &dyn HttpTransport,
            _request: UpstreamRequest,
        ) -> Result<keycompute_provider_trait::StreamBox> {
            let mut events: Vec<Result<StreamEvent>> = (0..self.chunks)
                .map(|_| {
                    Ok(StreamEvent::Delta {
                        content: "x".to_string(),
                        finish_reason: None,
                    })
                })
                .collect();

            events.push(Ok(StreamEvent::Usage {
                input_tokens: 1,
                output_tokens: self.chunks as u32,
            }));
            events.push(Ok(StreamEvent::Done));

            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[allow(dead_code)]
    fn create_test_context() -> RequestContext {
        RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-4o",
            vec![Message::user("Hello")],
            true,
            PricingSnapshot {
                model_name: "gpt-4o".to_string(),
                currency: "CNY".to_string(),
                input_price_per_1k: Decimal::from(1),
                output_price_per_1k: Decimal::from(2),
            },
        )
    }

    #[test]
    fn test_gateway_executor_new() {
        let config = GatewayConfig::default();
        let providers = HashMap::new();
        let executor = GatewayExecutor::new(config, providers);
        assert_eq!(executor.config.max_retries, 3);
    }

    #[test]
    fn test_estimate_tokens_english() {
        // 使用 tiktoken-rs o200k_base 精确计数
        // "Hello" = 1 token
        assert_eq!(GatewayExecutor::estimate_tokens("Hello"), 1);
        // "Hello World" = 2 tokens
        assert_eq!(GatewayExecutor::estimate_tokens("Hello World"), 2);
        // 100 个 'a' 约 25 tokens
        assert!(GatewayExecutor::estimate_tokens("a".repeat(100).as_str()) > 0);
    }

    #[test]
    fn test_estimate_tokens_chinese() {
        // 中文 token 计数（tiktoken 精确计算）
        // 中文字符通常每个 1-2 tokens
        assert!(GatewayExecutor::estimate_tokens("你好") > 0);
        assert!(GatewayExecutor::estimate_tokens("你好世界") > 0);
        assert!(GatewayExecutor::estimate_tokens("你好世界测试") > 0);
    }

    #[test]
    fn test_estimate_tokens_mixed() {
        // 混合：英文 + 中文
        assert!(GatewayExecutor::estimate_tokens("Hello你好") > 0);
        assert!(GatewayExecutor::estimate_tokens("Hello World你好世界") > 0);
    }

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(GatewayExecutor::estimate_tokens(""), 0);
    }

    #[tokio::test]
    async fn test_execute_returns_receiver_before_consuming_large_stream() {
        let config = GatewayConfig::default();
        let mut providers = HashMap::new();
        providers.insert(
            "many-chunks".to_string(),
            Arc::new(ManyChunksProvider { chunks: 150 }) as Arc<dyn ProviderAdapter>,
        );
        let executor = GatewayExecutor::new(config, providers);

        let ctx = Arc::new(create_test_context());
        let plan = ExecutionPlan {
            primary: ExecutionTarget {
                provider: "many-chunks".to_string(),
                account_id: Uuid::new_v4(),
                endpoint: "http://mock".to_string(),
                upstream_api_key: "mock-key".into(),
            },
            fallback_chain: vec![],
        };

        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());

        let mut rx = tokio::time::timeout(
            Duration::from_millis(50),
            executor.execute(ctx, plan, account_states, Some(provider_health)),
        )
        .await
        .expect("execute should return receiver immediately")
        .expect("execute should succeed");

        let first_event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("stream should produce events")
            .expect("channel should stay open");

        assert!(matches!(first_event, StreamEvent::Delta { .. }));
    }
}
