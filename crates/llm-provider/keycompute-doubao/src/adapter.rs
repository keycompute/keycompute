//! Doubao Provider Adapter 实现
//!
//! 复用 OpenAI 协议层，Doubao API 与 OpenAI API 高度兼容。
//! 主要差异：
//! - 默认端点: https://ark.cn-beijing.volces.com/api/v3/chat/completions
//! - 支持的模型: doubao-pro, doubao-lite, doubao-pro-32k, doubao-lite-32k 等
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

/// Doubao 默认 API 端点（火山引擎）
pub const DOUBAO_DEFAULT_ENDPOINT: &str = "https://ark.cn-beijing.volces.com/api/v3/chat/completions";

/// Doubao 支持的模型列表
pub const DOUBAO_MODELS: &[&str] = &[
    "doubao-pro",
    "doubao-pro-32k",
    "doubao-pro-128k",
    "doubao-lite",
    "doubao-lite-32k",
    "doubao-lite-128k",
    // DeepSeek 系列（通过火山引擎）
    "deepseek-r1",
    "deepseek-v3",
];

/// Doubao Provider 适配器
///
/// 基于 OpenAI 协议实现，复用 OpenAI 的请求/响应结构和流处理逻辑。
#[derive(Debug, Clone)]
pub struct DoubaoProvider;

impl Default for DoubaoProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DoubaoProvider {
    /// 创建新的 Doubao Provider
    pub fn new() -> Self {
        Self
    }

    /// 构建请求体（复用 OpenAI 格式）
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
    fn get_endpoint(&self, request: &UpstreamRequest) -> String {
        if request.endpoint.is_empty() {
            return DOUBAO_DEFAULT_ENDPOINT.to_string();
        }

        let endpoint = request.endpoint.clone();
        // 如果 endpoint 以 /v3 结尾，说明是基础 URL，需要拼接路径
        if endpoint.ends_with("/v3") || endpoint.ends_with("/v3/") {
            let base = endpoint.trim_end_matches('/');
            format!("{}/chat/completions", base)
        } else {
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

        let doubao_response: OpenAIResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to parse Doubao response: {}", e))
            })?;

        let content = doubao_response
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

        let headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
        ];

        let byte_stream: ByteStream = transport.post_stream(&endpoint, headers, body_json).await?;

        // 复用 OpenAI 的流解析器，Doubao SSE 格式与 OpenAI 完全兼容
        Ok(parse_openai_stream(byte_stream))
    }
}

#[async_trait]
impl ProviderAdapter for DoubaoProvider {
    fn name(&self) -> &'static str {
        "doubao"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        DOUBAO_MODELS.to_vec()
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
    fn test_doubao_provider_name() {
        let provider = DoubaoProvider::new();
        assert_eq!(provider.name(), "doubao");
    }

    #[test]
    fn test_doubao_supported_models() {
        let provider = DoubaoProvider::new();
        let models = provider.supported_models();
        assert!(models.contains(&"doubao-pro"));
        assert!(models.contains(&"doubao-lite"));
        assert!(models.contains(&"doubao-pro-32k"));
        assert!(models.contains(&"doubao-lite-128k"));
    }

    #[test]
    fn test_doubao_supports_model() {
        let provider = DoubaoProvider::new();
        assert!(provider.supports_model("doubao-pro"));
        assert!(provider.supports_model("doubao-lite-32k"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_default_endpoint() {
        assert_eq!(
            DOUBAO_DEFAULT_ENDPOINT,
            "https://ark.cn-beijing.volces.com/api/v3/chat/completions"
        );
    }

    #[test]
    fn test_build_request_body() {
        let provider = DoubaoProvider::new();
        let request = keycompute_provider_trait::UpstreamRequest::new(
            "https://ark.cn-beijing.volces.com/api/v3/chat/completions",
            "sk-test",
            "doubao-pro",
        )
        .with_message("system", "You are helpful")
        .with_message("user", "Hello")
        .with_stream(true)
        .with_max_tokens(100)
        .with_temperature(0.7);

        let body = provider.build_request_body(&request);

        assert_eq!(body.model, "doubao-pro");
        assert_eq!(body.messages.len(), 2);
        assert_eq!(body.stream, Some(true));
        assert_eq!(body.max_tokens, Some(100));
        assert_eq!(body.temperature, Some(0.7));
        assert!(body.stream_options.is_some());
        assert_eq!(
            body.stream_options.as_ref().unwrap().include_usage,
            Some(true)
        );
    }

    #[test]
    fn test_get_endpoint_default() {
        let provider = DoubaoProvider::new();
        let request = keycompute_provider_trait::UpstreamRequest::new(
            "", // 空端点
            "sk-test",
            "doubao-pro",
        );

        assert_eq!(provider.get_endpoint(&request), DOUBAO_DEFAULT_ENDPOINT);
    }

    #[test]
    fn test_get_endpoint_custom_full() {
        let provider = DoubaoProvider::new();
        let custom_endpoint = "https://custom.endpoint.com/v3/chat/completions";
        let request = keycompute_provider_trait::UpstreamRequest::new(
            custom_endpoint,
            "sk-test",
            "doubao-pro",
        );

        assert_eq!(provider.get_endpoint(&request), custom_endpoint);
    }

    #[test]
    fn test_get_endpoint_base_url() {
        let provider = DoubaoProvider::new();
        let request = keycompute_provider_trait::UpstreamRequest::new(
            "https://ark.cn-beijing.volces.com/api/v3",
            "sk-test",
            "doubao-pro",
        );

        let endpoint = provider.get_endpoint(&request);
        assert_eq!(
            endpoint,
            "https://ark.cn-beijing.volces.com/api/v3/chat/completions"
        );
    }
}
