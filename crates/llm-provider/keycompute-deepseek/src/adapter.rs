//! DeepSeek Provider Adapter 实现
//!
//! 复用 OpenAI 协议层，DeepSeek API 与 OpenAI API 高度兼容。
//! 主要差异：
//! - 默认端点: https://api.deepseek.com/v1/chat/completions
//! - 支持的模型: deepseek-chat, deepseek-coder, deepseek-reasoner
//!
//! 使用统一 HTTP 传输层：
//! - 通过 HttpTransport 发送请求
//! - 支持连接池复用和代理出口
//!
//! # 重要说明
//! - `endpoint` 和 `upstream_api_key` 由调用方通过 `UpstreamRequest` 传入
//! - 这些值通常从数据库 Account 表获取，而非配置文件
//! - 管理员可通过前端界面动态配置，无需重启系统

use async_trait::async_trait;
use futures::StreamExt;
use keycompute_openai::{
    OpenAIRequest, OpenAIResponse,
    protocol::{OpenAIMessage, StreamOptions},
    stream::parse_openai_stream,
};
use keycompute_provider_trait::{
    ByteStream, HttpTransport, ProviderAdapter, StreamBox, StreamEvent, UpstreamRequest,
};
use keycompute_types::{KeyComputeError, Result};
use serde_json;

/// DeepSeek 默认 API 端点
pub const DEEPSEEK_DEFAULT_ENDPOINT: &str = "https://api.deepseek.com/v1/chat/completions";

/// DeepSeek 支持的模型列表
pub const DEEPSEEK_MODELS: &[&str] = &[
    "deepseek-empty", // DeepSeek 示例空模型名称
];

/// DeepSeek Provider 适配器
///
/// 基于 OpenAI 协议实现，复用 OpenAI 的请求/响应结构和流处理逻辑。
#[derive(Debug, Clone)]
pub struct DeepSeekProvider;

impl Default for DeepSeekProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DeepSeekProvider {
    /// 创建新的 DeepSeek Provider
    pub fn new() -> Self {
        Self
    }

    /// 构建 DeepSeek 请求体
    ///
    /// 与 OpenAI 请求结构相同，但使用 DeepSeek 的模型名称
    fn build_request_body(&self, request: &UpstreamRequest) -> OpenAIRequest {
        let messages: Vec<OpenAIMessage> = request
            .messages
            .iter()
            .map(|m| OpenAIMessage {
                role: m.role.clone(),
                content: Some(m.content.clone()),
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
            stream_options: if request.stream {
                Some(StreamOptions {
                    include_usage: Some(true),
                })
            } else {
                None
            },
        }
    }

    /// 获取实际请求端点
    ///
    /// 支持两种格式的 endpoint 配置：
    /// - 完整路径: `https://api.deepseek.com/v1/chat/completions`
    /// - 基础 URL: `https://api.deepseek.com/v1` (会自动拼接 `/chat/completions`)
    fn get_endpoint(&self, request: &UpstreamRequest) -> String {
        if request.endpoint.is_empty() {
            return DEEPSEEK_DEFAULT_ENDPOINT.to_string();
        }

        let endpoint = request.endpoint.clone();
        // 如果 endpoint 以 /v1 或 /v1/ 结尾，说明是基础 URL，需要拼接路径
        if endpoint.ends_with("/v1") || endpoint.ends_with("/v1/") {
            let base = endpoint.trim_end_matches('/');
            format!("{}/chat/completions", base)
        } else {
            // 否则假设用户提供了完整路径
            endpoint
        }
    }

    /// 执行非流式请求
    async fn chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<String> {
        let body = self.build_request_body(&request);
        let endpoint = self.get_endpoint(&request);
        let body_json = serde_json::to_string(&body).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to serialize request: {}", e))
        })?;

        let headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];

        let response_text = transport.post_json(&endpoint, headers, body_json).await?;

        let deepseek_response: OpenAIResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to parse DeepSeek response: {}", e))
            })?;

        let content = deepseek_response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        Ok(content)
    }

    /// 执行流式请求
    async fn stream_chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<StreamBox> {
        let body = self.build_request_body(&request);
        let endpoint = self.get_endpoint(&request);
        let body_json = serde_json::to_string(&body).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to serialize request: {}", e))
        })?;

        eprintln!(
            "[DEBUG] DeepSeek stream_chat_internal: endpoint={}",
            endpoint
        );

        let headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
        ];

        eprintln!("[DEBUG] DeepSeek stream_chat_internal: calling transport.post_stream");
        let byte_stream: ByteStream = transport.post_stream(&endpoint, headers, body_json).await?;
        eprintln!("[DEBUG] DeepSeek stream_chat_internal: post_stream returned, creating parser");

        // 复用 OpenAI 的流解析器，DeepSeek SSE 格式与 OpenAI 完全兼容
        Ok(parse_openai_stream(byte_stream))
    }
}

#[async_trait]
impl ProviderAdapter for DeepSeekProvider {
    fn name(&self) -> &'static str {
        "deepseek"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        DEEPSEEK_MODELS.to_vec()
    }

    async fn stream_chat(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<StreamBox> {
        if request.stream {
            self.stream_chat_internal(transport, request).await
        } else {
            // 非流式请求，包装为单事件流
            let content = self.chat_internal(transport, request).await?;
            let event = StreamEvent::delta(content);

            let stream = futures::stream::once(async move { Ok(event) }).chain(
                futures::stream::once(async move { Ok(StreamEvent::done()) }),
            );

            Ok(Box::pin(stream))
        }
    }

    async fn chat(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<String> {
        self.chat_internal(transport, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deepseek_provider_name() {
        let provider = DeepSeekProvider::new();
        assert_eq!(provider.name(), "deepseek");
    }

    #[test]
    fn test_deepseek_supported_models() {
        let provider = DeepSeekProvider::new();
        let models = provider.supported_models();
        assert!(models.contains(&"deepseek-empty"));
        assert_eq!(models.len(), 1);
    }

    #[test]
    fn test_deepseek_supports_model() {
        let provider = DeepSeekProvider::new();
        assert!(provider.supports_model("deepseek-empty"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_default_endpoint() {
        assert_eq!(
            DEEPSEEK_DEFAULT_ENDPOINT,
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_request_body() {
        let provider = DeepSeekProvider::new();
        let request = keycompute_provider_trait::UpstreamRequest::new(
            "https://api.deepseek.com/v1/chat/completions",
            "sk-test",
            "deepseek-chat",
        )
        .with_message("system", "You are helpful")
        .with_message("user", "Hello")
        .with_stream(true)
        .with_max_tokens(100)
        .with_temperature(0.7);

        let body = provider.build_request_body(&request);

        assert_eq!(body.model, "deepseek-chat");
        assert_eq!(body.messages.len(), 2);
        assert_eq!(body.stream, Some(true));
        assert_eq!(body.max_tokens, Some(100));
        assert_eq!(body.temperature, Some(0.7));
        // 验证 stream_options 包含 usage
        assert!(body.stream_options.is_some());
        assert_eq!(
            body.stream_options.as_ref().unwrap().include_usage,
            Some(true)
        );
    }

    #[test]
    fn test_get_endpoint_default() {
        let provider = DeepSeekProvider::new();
        let request = keycompute_provider_trait::UpstreamRequest::new(
            "", // 空端点
            "sk-test",
            "deepseek-chat",
        );

        assert_eq!(provider.get_endpoint(&request), DEEPSEEK_DEFAULT_ENDPOINT);
    }

    #[test]
    fn test_get_endpoint_custom() {
        let provider = DeepSeekProvider::new();
        let custom_endpoint = "https://custom.deepseek.com/v1/chat/completions";
        let request = keycompute_provider_trait::UpstreamRequest::new(
            custom_endpoint,
            "sk-test",
            "deepseek-chat",
        );

        assert_eq!(provider.get_endpoint(&request), custom_endpoint);
    }
}
