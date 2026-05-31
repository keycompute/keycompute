//! vLLM Provider Adapter 实现
//!
//! 复用 OpenAI 协议层，vLLM 提供 OpenAI 兼容的 API。
//! 主要差异：
//! - 默认端点: http://localhost:8000/v1/chat/completions
//! - 支持任意 HuggingFace 模型（动态加载）
//!
//! vLLM 特点：
//! - 高性能推理（PagedAttention、连续批处理）
//! - 支持流式输出
//! - 完全 OpenAI API 兼容
//!
//! 使用统一 HTTP 传输层：
//! - 通过 HttpTransport 发送请求
//! - 支持连接池复用和代理出口

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

/// vLLM 默认 API 端点（本地部署）
pub const VLLM_DEFAULT_ENDPOINT: &str = "http://localhost:8000/v1/chat/completions";

/// vLLM 常用模型示例
///
/// 注意：vLLM 支持加载任意 HuggingFace 模型，这里只列出常见示例
pub const VLLM_COMMON_MODELS: &[&str] = &[
    // Meta LLaMA 系列
    "meta-llama/Llama-3.1-8B-Instruct",
    "meta-llama/Llama-3.1-70B-Instruct",
    "meta-llama/Llama-3.2-1B-Instruct",
    "meta-llama/Llama-3.2-3B-Instruct",
    // Qwen 系列
    "Qwen/Qwen2.5-7B-Instruct",
    "Qwen/Qwen2.5-72B-Instruct",
    // Mistral 系列
    "mistralai/Mistral-7B-Instruct-v0.3",
    "mistralai/Mixtral-8x7B-Instruct-v0.1",
    // 其他常见模型
    "deepseek-ai/deepseek-coder-33b-instruct",
    "THUDM/chatglm3-6b",
];

/// vLLM Provider 适配器
///
/// 基于 OpenAI 协议实现，复用 OpenAI 的请求/响应结构和流处理逻辑。
/// vLLM 提供 OpenAI 兼容的 API，因此可以直接复用协议层。
#[derive(Debug, Clone)]
pub struct VllmProvider {
    /// 支持的模型列表（动态配置）
    supported_models: Vec<String>,
}

impl Default for VllmProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl VllmProvider {
    /// 创建新的 vLLM Provider
    pub fn new() -> Self {
        Self {
            supported_models: VLLM_COMMON_MODELS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// 创建带自定义模型列表的 Provider
    pub fn with_models(models: Vec<String>) -> Self {
        Self {
            supported_models: models,
        }
    }

    /// 构建 vLLM 请求体
    ///
    /// 与 OpenAI 请求结构相同，vLLM 完全兼容
    fn build_request_body(&self, request: &UpstreamRequest) -> OpenAIRequest {
        let messages: Vec<OpenAIMessage> = request
            .messages
            .iter()
            .map(|m| OpenAIMessage {
                role: m.role.clone(),
                content: Some(keycompute_openai::convert_message_content(
                    m.content.clone(),
                )),
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
            VLLM_DEFAULT_ENDPOINT.to_string()
        } else {
            request.endpoint.clone()
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
            ("Content-Type".to_string(), "application/json".to_string()),
            // vLLM 本地部署通常不需要 API Key，但支持可选的 Authorization
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
        ];

        let response_text = transport.post_json(&endpoint, headers, body_json).await?;

        let vllm_response: OpenAIResponse = serde_json::from_str(&response_text).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to parse vLLM response: {}", e))
        })?;

        let content = vllm_response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .map(|c| c.extract_text())
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
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
            (
                "Authorization".to_string(),
                format!("Bearer {}", request.upstream_api_key.expose()),
            ),
        ];

        let byte_stream: ByteStream = transport.post_stream(&endpoint, headers, body_json).await?;
        // 复用 OpenAI 的流解析器，vLLM SSE 格式与 OpenAI 完全兼容
        Ok(parse_openai_stream(byte_stream))
    }

    /// 添加支持的模型
    pub fn add_model(&mut self, model: impl Into<String>) {
        let model = model.into();
        if !self.supported_models.contains(&model) {
            self.supported_models.push(model);
        }
    }

    /// 设置支持的模型列表
    pub fn set_supported_models(&mut self, models: Vec<String>) {
        self.supported_models = models;
    }
}

#[async_trait]
impl ProviderAdapter for VllmProvider {
    fn name(&self) -> &'static str {
        "vllm"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        // 由于 supported_models 是 Vec<String>，需要转换
        // 这里返回预定义的常见模型列表
        VLLM_COMMON_MODELS.to_vec()
    }

    fn supports_model(&self, model: &str) -> bool {
        // vLLM 支持动态加载模型，所以默认返回 true
        // 实际部署时可能需要检查服务端支持的模型
        self.supported_models.iter().any(|m| m == model)
            || model.contains('/') // HuggingFace 模型格式 (org/model)
            || model.starts_with("local:") // 本地模型
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
    fn test_vllm_provider_name() {
        let provider = VllmProvider::new();
        assert_eq!(provider.name(), "vllm");
    }

    #[test]
    fn test_vllm_supported_models() {
        let provider = VllmProvider::new();
        let models = provider.supported_models();
        assert!(models.contains(&"meta-llama/Llama-3.1-8B-Instruct"));
        assert!(models.contains(&"Qwen/Qwen2.5-7B-Instruct"));
    }

    #[test]
    fn test_vllm_supports_model() {
        let provider = VllmProvider::new();
        // 预定义模型
        assert!(provider.supports_model("meta-llama/Llama-3.1-8B-Instruct"));
        // HuggingFace 格式
        assert!(provider.supports_model("some-org/some-model"));
        // 本地模型
        assert!(provider.supports_model("local:/path/to/model"));
        // 不支持的模型
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_default_endpoint() {
        assert_eq!(
            VLLM_DEFAULT_ENDPOINT,
            "http://localhost:8000/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_request_body() {
        let provider = VllmProvider::new();
        let request = keycompute_provider_trait::UpstreamRequest::new(
            "http://localhost:8000/v1/chat/completions",
            "", // vLLM 本地部署通常不需要 API Key
            "meta-llama/Llama-3.1-8B-Instruct",
        )
        .with_message("system", "You are helpful")
        .with_message("user", "Hello")
        .with_stream(true)
        .with_max_tokens(100)
        .with_temperature(0.7);

        let body = provider.build_request_body(&request);

        assert_eq!(body.model, "meta-llama/Llama-3.1-8B-Instruct");
        assert_eq!(body.messages.len(), 2);
        assert_eq!(body.stream, Some(true));
        assert_eq!(body.max_tokens, Some(100));
        assert_eq!(body.temperature, Some(0.7));
        // 验证 stream_options 包含 usage
        assert!(body.stream_options.is_some());
    }

    #[test]
    fn test_get_endpoint_default() {
        let provider = VllmProvider::new();
        let request = keycompute_provider_trait::UpstreamRequest::new(
            "", // 空端点
            "",
            "meta-llama/Llama-3.1-8B-Instruct",
        );

        assert_eq!(provider.get_endpoint(&request), VLLM_DEFAULT_ENDPOINT);
    }

    #[test]
    fn test_get_endpoint_custom() {
        let provider = VllmProvider::new();
        let custom_endpoint = "http://192.168.1.100:8000/v1/chat/completions";
        let request = keycompute_provider_trait::UpstreamRequest::new(
            custom_endpoint,
            "",
            "meta-llama/Llama-3.1-8B-Instruct",
        );

        assert_eq!(provider.get_endpoint(&request), custom_endpoint);
    }

    #[test]
    fn test_with_models() {
        let custom_models = vec!["custom/model-1".to_string(), "custom/model-2".to_string()];
        let provider = VllmProvider::with_models(custom_models);

        assert!(provider.supports_model("custom/model-1"));
        assert!(provider.supports_model("custom/model-2"));
    }

    #[test]
    fn test_add_model() {
        let mut provider = VllmProvider::new();
        provider.add_model("custom/new-model");

        assert!(provider.supports_model("custom/new-model"));
    }
}
