//! Ollama Provider Adapter 实现
//!
//! 实现 ProviderAdapter trait，提供 Ollama 本地模型的调用能力
//!
//! Ollama 支持两种 API 格式：
//! 1. 原生格式: POST /api/chat (本实现使用此格式)
//! 2. OpenAI 兼容格式: POST /v1/chat/completions
//!
//! 使用统一 HTTP 传输层：
//! - 通过 HttpTransport 发送请求
//! - 支持连接池复用和代理出口

use async_trait::async_trait;
use futures::StreamExt;
use keycompute_provider_trait::{
    ByteStream, HttpTransport, ProviderAdapter, StreamBox, StreamEvent, UpstreamRequest,
};
use keycompute_types::{KeyComputeError, Result};
use serde_json;

use crate::protocol::{OllamaMessage, OllamaOptions, OllamaRequest, OllamaResponse};
use crate::stream::parse_ollama_stream;

/// Ollama 默认 API 端点
pub const OLLAMA_DEFAULT_ENDPOINT: &str = "http://localhost:11434/api/chat";

/// Ollama 支持的模型列表（常见模型）
pub const OLLAMA_MODELS: &[&str] = &[
    // Meta Llama 系列
    "llama3.2",
    "llama3.2:1b",
    "llama3.2:3b",
    "llama3.1",
    "llama3.1:8b",
    "llama3.1:70b",
    "llama3.1:405b",
    "llama2",
    "llama2:7b",
    "llama2:13b",
    "llama2:70b",
    // Mistral 系列
    "mistral",
    "mistral:7b",
    "mistral-nemo",
    "mixtral",
    "mixtral:8x7b",
    "mixtral:8x22b",
    // Google Gemma 系列
    "gemma2",
    "gemma2:2b",
    "gemma2:9b",
    "gemma2:27b",
    "gemma",
    "gemma:2b",
    "gemma:7b",
    // Qwen 系列
    "qwen2.5",
    "qwen2.5:0.5b",
    "qwen2.5:1.5b",
    "qwen2.5:7b",
    "qwen2.5:14b",
    "qwen2.5:32b",
    "qwen2.5:72b",
    // DeepSeek 系列
    "deepseek-r1",
    "deepseek-r1:1.5b",
    "deepseek-r1:7b",
    "deepseek-r1:8b",
    "deepseek-r1:14b",
    "deepseek-v2",
    // 其他常见模型
    "phi3",
    "phi3:3.8b",
    "phi3:14b",
    "codellama",
    "codellama:7b",
    "codellama:13b",
    "codellama:34b",
    "starcoder2",
    "starcoder2:3b",
    "starcoder2:7b",
    "starcoder2:15b",
];

/// Ollama Provider 适配器
#[derive(Debug, Clone)]
pub struct OllamaProvider;

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OllamaProvider {
    /// 创建新的 Ollama Provider
    pub fn new() -> Self {
        Self
    }

    /// 构建 Ollama 请求体
    fn build_request_body(&self, request: &UpstreamRequest) -> OllamaRequest {
        let mut system_content = None;
        let mut messages = Vec::new();

        for msg in &request.messages {
            if msg.role == "system" {
                system_content = Some(msg.content.clone());
            } else {
                messages.push(OllamaMessage {
                    role: msg.role.clone(),
                    content: msg.content.clone(),
                    images: None,
                });
            }
        }

        let options = if request.temperature.is_some()
            || request.top_p.is_some()
            || request.max_tokens.is_some()
        {
            let mut opts = OllamaOptions::new();
            if let Some(temp) = request.temperature {
                opts = opts.temperature(temp);
            }
            if let Some(top_p) = request.top_p {
                opts = opts.top_p(top_p);
            }
            if let Some(max_tokens) = request.max_tokens {
                opts = opts.num_predict(max_tokens as i32);
            }
            Some(opts)
        } else {
            None
        };

        OllamaRequest {
            model: request.model.clone(),
            messages,
            stream: Some(request.stream),
            format: None,
            options,
            system: system_content,
        }
    }

    fn get_endpoint(&self, request: &UpstreamRequest) -> String {
        if request.endpoint.is_empty() {
            OLLAMA_DEFAULT_ENDPOINT.to_string()
        } else {
            request.endpoint.clone()
        }
    }

    fn build_headers(&self, api_key: &str) -> Vec<(String, String)> {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        if !api_key.is_empty() && api_key != "mock-api-key" {
            headers.push(("Authorization".to_string(), format!("Bearer {}", api_key)));
        }
        headers
    }

    async fn chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<String> {
        let body = self.build_request_body(&request);
        let body = OllamaRequest {
            stream: Some(false),
            ..body
        };
        let endpoint = self.get_endpoint(&request);
        let body_json = serde_json::to_string(&body).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to serialize request: {}", e))
        })?;

        let headers = self.build_headers(&request.upstream_api_key);
        let response_text = transport.post_json(&endpoint, headers, body_json).await?;
        let ollama_response: OllamaResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to parse Ollama response: {}", e))
            })?;

        Ok(ollama_response.extract_text().to_string())
    }

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

        let headers = self.build_headers(&request.upstream_api_key);
        let byte_stream: ByteStream = transport.post_stream(&endpoint, headers, body_json).await?;
        Ok(parse_ollama_stream(byte_stream))
    }
}

#[async_trait]
impl ProviderAdapter for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        OLLAMA_MODELS.to_vec()
    }

    async fn stream_chat(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<StreamBox> {
        if request.stream {
            self.stream_chat_internal(transport, request).await
        } else {
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
    fn test_ollama_provider_name() {
        let provider = OllamaProvider::new();
        assert_eq!(provider.name(), "ollama");
    }

    #[test]
    fn test_ollama_supported_models() {
        let provider = OllamaProvider::new();
        let models = provider.supported_models();
        assert!(models.contains(&"llama3.2"));
        assert!(models.contains(&"mistral"));
        assert!(models.contains(&"gemma2"));
    }

    #[test]
    fn test_ollama_supports_model() {
        let provider = OllamaProvider::new();
        assert!(provider.supports_model("llama3.2"));
        assert!(provider.supports_model("mistral"));
        assert!(provider.supports_model("qwen2.5:7b"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_default_endpoint() {
        assert_eq!(OLLAMA_DEFAULT_ENDPOINT, "http://localhost:11434/api/chat");
    }

    #[test]
    fn test_build_request_body() {
        let provider = OllamaProvider::new();
        let request = UpstreamRequest::new("http://localhost:11434/api/chat", "", "llama3.2")
            .with_message("system", "You are helpful")
            .with_message("user", "Hello")
            .with_stream(true)
            .with_temperature(0.7);

        let body = provider.build_request_body(&request);
        assert_eq!(body.model, "llama3.2");
        assert_eq!(body.system, Some("You are helpful".to_string()));
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.stream, Some(true));
    }

    #[test]
    fn test_get_endpoint_default() {
        let provider = OllamaProvider::new();
        let request = UpstreamRequest::new("", "", "llama3.2");
        assert_eq!(provider.get_endpoint(&request), OLLAMA_DEFAULT_ENDPOINT);
    }

    #[test]
    fn test_build_headers_no_auth() {
        let provider = OllamaProvider::new();
        let headers = provider.build_headers("");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "Content-Type" && v == "application/json")
        );
        assert!(!headers.iter().any(|(k, _)| k == "Authorization"));
    }

    #[test]
    fn test_build_headers_with_auth() {
        let provider = OllamaProvider::new();
        let headers = provider.build_headers("sk-test-key");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer sk-test-key")
        );
    }
}
