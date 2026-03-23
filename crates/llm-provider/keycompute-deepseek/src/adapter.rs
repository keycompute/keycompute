//! DeepSeek Provider Adapter 实现
//!
//! 复用 OpenAI 协议层，DeepSeek API 与 OpenAI API 高度兼容。
//! 主要差异：
//! - 默认端点: https://api.deepseek.com/v1/chat/completions
//! - 支持的模型: deepseek-chat, deepseek-coder, deepseek-reasoner

use async_trait::async_trait;
use futures::StreamExt;
use keycompute_openai::{
    OpenAIRequest, OpenAIResponse,
    protocol::{OpenAIMessage, StreamOptions},
    stream::parse_openai_stream,
};
use keycompute_provider_trait::{ProviderAdapter, StreamBox, StreamEvent, UpstreamRequest};
use keycompute_types::{KeyComputeError, Result};
use reqwest::Client;
use std::time::Duration;

/// DeepSeek 默认 API 端点
pub const DEEPSEEK_DEFAULT_ENDPOINT: &str = "https://api.deepseek.com/v1/chat/completions";

/// DeepSeek 支持的模型列表
pub const DEEPSEEK_MODELS: &[&str] = &[
    "deepseek-chat",
    "deepseek-coder", 
    "deepseek-reasoner",
    // 兼容旧版本模型名称
    "deepseek-chat-pro",
    "deepseek-coder-pro",
];

/// DeepSeek Provider 适配器
///
/// 基于 OpenAI 协议实现，复用 OpenAI 的请求/响应结构和流处理逻辑。
#[derive(Debug, Clone)]
pub struct DeepSeekProvider {
    /// 默认端点
    default_endpoint: String,
    /// HTTP 客户端
    client: Client,
    /// 请求超时
    timeout: Duration,
}

impl Default for DeepSeekProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DeepSeekProvider {
    /// 创建新的 DeepSeek Provider
    pub fn new() -> Self {
        Self {
            default_endpoint: DEEPSEEK_DEFAULT_ENDPOINT.to_string(),
            client: Client::new(),
            timeout: Duration::from_secs(120),
        }
    }

    /// 创建带自定义端点的 Provider
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            default_endpoint: endpoint.into(),
            client: Client::new(),
            timeout: Duration::from_secs(120),
        }
    }

    /// 创建带自定义超时的 Provider
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            default_endpoint: DEEPSEEK_DEFAULT_ENDPOINT.to_string(),
            client: Client::new(),
            timeout,
        }
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
    /// 如果 UpstreamRequest 中未指定端点，使用 DeepSeek 默认端点
    fn get_endpoint<'a>(&'a self, request: &'a UpstreamRequest) -> &'a str {
        if request.endpoint.is_empty() {
            &self.default_endpoint
        } else {
            &request.endpoint
        }
    }

    /// 执行非流式请求
    async fn chat_internal(&self, request: UpstreamRequest) -> Result<String> {
        let body = self.build_request_body(&request);
        let endpoint = self.get_endpoint(&request);

        let response = self
            .client
            .post(endpoint)
            .header("Authorization", format!("Bearer {}", request.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| KeyComputeError::ProviderError(format!("DeepSeek request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(KeyComputeError::ProviderError(format!(
                "DeepSeek API error ({}): {}",
                status, error_text
            )));
        }

        let deepseek_response: OpenAIResponse = response.json().await.map_err(|e| {
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
    async fn stream_chat_internal(&self, request: UpstreamRequest) -> Result<StreamBox> {
        let body = self.build_request_body(&request);
        let endpoint = self.get_endpoint(&request);

        let response = self
            .client
            .post(endpoint)
            .header("Authorization", format!("Bearer {}", request.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| KeyComputeError::ProviderError(format!("DeepSeek request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(KeyComputeError::ProviderError(format!(
                "DeepSeek API error ({}): {}",
                status, error_text
            )));
        }

        let stream = response.bytes_stream();
        // 复用 OpenAI 的流解析器，DeepSeek SSE 格式与 OpenAI 完全兼容
        Ok(parse_openai_stream(stream))
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

    async fn stream_chat(&self, request: UpstreamRequest) -> Result<StreamBox> {
        if request.stream {
            self.stream_chat_internal(request).await
        } else {
            // 非流式请求，包装为单事件流
            let content = self.chat_internal(request).await?;
            let event = StreamEvent::delta(content);

            let stream = futures::stream::once(async move { Ok(event) }).chain(
                futures::stream::once(async move { Ok(StreamEvent::done()) }),
            );

            Ok(Box::pin(stream))
        }
    }

    async fn chat(&self, request: UpstreamRequest) -> Result<String> {
        self.chat_internal(request).await
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
        assert!(models.contains(&"deepseek-chat"));
        assert!(models.contains(&"deepseek-coder"));
        assert!(models.contains(&"deepseek-reasoner"));
    }

    #[test]
    fn test_deepseek_supports_model() {
        let provider = DeepSeekProvider::new();
        assert!(provider.supports_model("deepseek-chat"));
        assert!(provider.supports_model("deepseek-coder"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_default_endpoint() {
        assert_eq!(DEEPSEEK_DEFAULT_ENDPOINT, "https://api.deepseek.com/v1/chat/completions");
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
        assert_eq!(body.stream_options.as_ref().unwrap().include_usage, Some(true));
    }

    #[test]
    fn test_get_endpoint_default() {
        let provider = DeepSeekProvider::new();
        let request = keycompute_provider_trait::UpstreamRequest::new(
            "",  // 空端点
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
