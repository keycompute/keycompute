//! OpenAI Provider Adapter 实现
//!
//! 实现 ProviderAdapter trait，提供 OpenAI API 的调用能力

use async_trait::async_trait;
use futures::StreamExt;
use keycompute_provider_trait::{
    ProviderAdapter, StreamBox, StreamEvent, UpstreamRequest,
};
use keycompute_types::{KeyComputeError, Result};
use reqwest::Client;
use std::time::Duration;

use crate::protocol::{OpenAIRequest, OpenAIResponse, OpenAIMessage, StreamOptions};
use crate::stream::parse_openai_stream;

/// OpenAI Provider 适配器
#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    client: Client,
    timeout: Duration,
}

impl Default for OpenAIProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAIProvider {
    /// 创建新的 OpenAI Provider
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            timeout: Duration::from_secs(120),
        }
    }

    /// 创建带自定义超时的 Provider
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            client: Client::new(),
            timeout,
        }
    }

    /// 构建 OpenAI 请求体
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

    /// 执行非流式请求
    async fn chat_internal(&self, request: UpstreamRequest) -> Result<String> {
        let body = self.build_request_body(&request);

        let response = self
            .client
            .post(&request.endpoint)
            .header("Authorization", format!("Bearer {}", request.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| KeyComputeError::ProviderError(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(KeyComputeError::ProviderError(format!(
                "OpenAI API error ({}): {}",
                status, error_text
            )));
        }

        let openai_response: OpenAIResponse = response
            .json()
            .await
            .map_err(|e| KeyComputeError::ProviderError(format!("Failed to parse response: {}", e)))?;

        // 提取内容
        let content = openai_response
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

        let response = self
            .client
            .post(&request.endpoint)
            .header("Authorization", format!("Bearer {}", request.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| KeyComputeError::ProviderError(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(KeyComputeError::ProviderError(format!(
                "OpenAI API error ({}): {}",
                status, error_text
            )));
        }

        let stream = response.bytes_stream();
        Ok(parse_openai_stream(stream))
    }
}

#[async_trait]
impl ProviderAdapter for OpenAIProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        vec![
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4-turbo",
            "gpt-4",
            "gpt-3.5-turbo",
            "gpt-3.5-turbo-16k",
        ]
    }

    async fn stream_chat(&self, request: UpstreamRequest) -> Result<StreamBox> {
        if request.stream {
            self.stream_chat_internal(request).await
        } else {
            // 非流式请求，包装为单事件流
            let content = self.chat_internal(request).await?;
            let event = StreamEvent::delta(content);

            let stream = futures::stream::once(async move { Ok(event) })
                .chain(futures::stream::once(async move { Ok(StreamEvent::done()) }));

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
    fn test_openai_provider_name() {
        let provider = OpenAIProvider::new();
        assert_eq!(provider.name(), "openai");
    }

    #[test]
    fn test_openai_supported_models() {
        let provider = OpenAIProvider::new();
        let models = provider.supported_models();
        assert!(models.contains(&"gpt-4o"));
        assert!(models.contains(&"gpt-3.5-turbo"));
    }

    #[test]
    fn test_openai_supports_model() {
        let provider = OpenAIProvider::new();
        assert!(provider.supports_model("gpt-4o"));
        assert!(provider.supports_model("gpt-4o-mini"));
        assert!(!provider.supports_model("unknown-model"));
    }

    #[test]
    fn test_build_request_body() {
        let provider = OpenAIProvider::new();
        let request = UpstreamRequest::new(
            "https://api.openai.com/v1/chat/completions",
            "sk-test",
            "gpt-4o",
        )
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
