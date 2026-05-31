//! Google Gemini Provider Adapter 实现
//!
//! 实现 ProviderAdapter trait，提供 Google Gemini API 的调用能力
//!
//! Gemini API 与 OpenAI API 的主要差异：
//! - 端点: https://generativelanguage.googleapis.com/v1beta/models/{model}:{method}
//! - 认证: query parameter `key={api_key}` 而非 Bearer token
//! - 请求结构: contents 数组而非 messages，systemInstruction 为独立字段
//! - 响应结构: candidates 数组而非 choices
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
use keycompute_provider_trait::{
    ByteStream, HttpTransport, ProviderAdapter, StreamBox, StreamEvent, UpstreamRequest,
};
use keycompute_types::{KeyComputeError, MessageContent, Result};
use serde_json;

use crate::protocol::{
    GEMINI_DEFAULT_ENDPOINT, GEMINI_MODELS, GeminiContent, GeminiPart, GeminiRequest,
    GeminiResponse, GenerationConfig,
};
use crate::stream::parse_gemini_stream;

/// Gemini Provider 适配器
#[derive(Debug, Clone)]
pub struct GeminiProvider;

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiProvider {
    /// 创建新的 Gemini Provider
    pub fn new() -> Self {
        Self
    }

    /// 构建 Gemini 请求体
    fn build_request_body(&self, request: &UpstreamRequest) -> Result<GeminiRequest> {
        // 分离 system 消息和普通消息
        let mut system_content = None;
        let mut contents = Vec::new();

        for (msg_idx, msg) in request.messages.iter().enumerate() {
            if msg.role == "system" {
                system_content = Some(msg.content.to_string());
            } else {
                // 检查是否包含 Vision 内容（Gemini 适配器当前不支持）
                match &msg.content {
                    MessageContent::Parts(_) => {
                        return Err(KeyComputeError::ProviderError(format!(
                            "Gemini adapter does not currently support Vision/multimodal content (message index {})",
                            msg_idx
                        )));
                    }
                    MessageContent::Text(_) => {}
                }
                // 转换角色: OpenAI 的 "assistant" -> Gemini 的 "model"
                let role = if msg.role == "assistant" {
                    "model"
                } else {
                    "user"
                };

                contents.push(GeminiContent {
                    role: role.to_string(),
                    parts: vec![GeminiPart {
                        text: Some(msg.content.to_string()),
                        inlineData: None,
                    }],
                });
            }
        }

        // 构建生成配置
        let mut config = GenerationConfig::default();
        if let Some(temp) = request.temperature {
            config.temperature = Some(temp);
        }
        if let Some(max_tokens) = request.max_tokens {
            config.maxOutputTokens = Some(max_tokens);
        }
        if let Some(top_p) = request.top_p {
            config.topP = Some(top_p);
        }

        let mut gemini_request = GeminiRequest::new();

        // 设置系统指令
        if let Some(system) = system_content {
            gemini_request = gemini_request.with_system_instruction(system);
        }

        // 添加消息内容
        for content in contents {
            gemini_request.contents.push(content);
        }

        // 设置生成配置（如果有非默认值）
        if config.temperature.is_some() || config.maxOutputTokens.is_some() || config.topP.is_some()
        {
            gemini_request.generationConfig = Some(config);
        }

        Ok(gemini_request)
    }

    /// 获取实际请求端点
    ///
    /// Gemini API 端点格式:
    /// - 流式: https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent?key={api_key}
    /// - 非流式: https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}
    fn get_endpoint(&self, request: &UpstreamRequest, stream: bool) -> String {
        let base = if request.endpoint.is_empty() {
            GEMINI_DEFAULT_ENDPOINT
        } else {
            &request.endpoint
        };

        // 提取模型名称（移除可能的前缀）
        let model = request.model.trim_start_matches("models/");

        let method = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };

        // Gemini 需要 API key 作为 query parameter
        format!(
            "{}/models/{}:{}?key={}",
            base,
            model,
            method,
            request.upstream_api_key.expose()
        )
    }

    /// 执行非流式请求
    async fn chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<String> {
        let body = self.build_request_body(&request)?;
        let endpoint = self.get_endpoint(&request, false);

        let body_json = serde_json::to_string(&body).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to serialize request: {}", e))
        })?;

        // Gemini 使用 query parameter 认证，不需要 Authorization header
        let headers = vec![("Content-Type".to_string(), "application/json".to_string())];

        let response_text = transport.post_json(&endpoint, headers, body_json).await?;

        let gemini_response: GeminiResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to parse Gemini response: {}", e))
            })?;

        // 提取文本内容
        let content = gemini_response.extract_text();
        Ok(content)
    }

    /// 执行流式请求
    async fn stream_chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<StreamBox> {
        let body = self.build_request_body(&request)?;
        let endpoint = self.get_endpoint(&request, true);

        let body_json = serde_json::to_string(&body).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to serialize request: {}", e))
        })?;

        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
        ];

        let byte_stream: ByteStream = transport.post_stream(&endpoint, headers, body_json).await?;

        // 转换为标准化的 StreamEvent 流
        Ok(parse_gemini_stream(byte_stream))
    }
}

#[async_trait]
impl ProviderAdapter for GeminiProvider {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        GEMINI_MODELS.to_vec()
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
    use keycompute_provider_trait::UpstreamMessage;

    #[test]
    fn test_gemini_provider_name() {
        let provider = GeminiProvider::new();
        assert_eq!(provider.name(), "gemini");
    }

    #[test]
    fn test_gemini_supported_models() {
        let provider = GeminiProvider::new();
        let models = provider.supported_models();
        assert!(models.contains(&"gemini-empty"));
        assert_eq!(models.len(), 1);
    }

    #[test]
    fn test_gemini_supports_model() {
        let provider = GeminiProvider::new();
        assert!(provider.supports_model("gemini-empty"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_build_request_body() {
        let provider = GeminiProvider::new();
        let request = UpstreamRequest::new(
            "https://generativelanguage.googleapis.com/v1beta",
            "test-api-key",
            "gemini-1.5-flash",
        )
        .with_message("system", "You are helpful")
        .with_message("user", "Hello")
        .with_stream(true)
        .with_temperature(0.7)
        .with_max_tokens(1024);

        let body = provider.build_request_body(&request).unwrap();

        // 验证系统指令
        assert!(body.systemInstruction.is_some());
        assert_eq!(body.contents.len(), 1); // 只有 user 消息

        // 验证生成配置
        let config = body.generationConfig.unwrap();
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.maxOutputTokens, Some(1024));
    }

    #[test]
    fn test_get_endpoint_non_stream() {
        let provider = GeminiProvider::new();
        let request = UpstreamRequest::new(
            "https://generativelanguage.googleapis.com/v1beta",
            "test-key",
            "gemini-1.5-flash",
        );

        let endpoint = provider.get_endpoint(&request, false);
        assert!(endpoint.contains("generateContent"));
        assert!(endpoint.contains("key=test-key"));
    }

    #[test]
    fn test_get_endpoint_stream() {
        let provider = GeminiProvider::new();
        let request = UpstreamRequest::new(
            "https://generativelanguage.googleapis.com/v1beta",
            "test-key",
            "gemini-1.5-flash",
        );

        let endpoint = provider.get_endpoint(&request, true);
        assert!(endpoint.contains("streamGenerateContent"));
        assert!(endpoint.contains("key=test-key"));
    }

    #[test]
    fn test_build_request_body_role_conversion() {
        let provider = GeminiProvider::new();
        let request = UpstreamRequest {
            endpoint: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            upstream_api_key: "test-key".into(),
            model: "gemini-1.5-flash".to_string(),
            messages: vec![
                UpstreamMessage {
                    role: "system".to_string(),
                    content: MessageContent::text("You are helpful"),
                },
                UpstreamMessage {
                    role: "user".to_string(),
                    content: MessageContent::text("Hello"),
                },
                UpstreamMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::text("Hi there!"),
                },
                UpstreamMessage {
                    role: "user".to_string(),
                    content: MessageContent::text("How are you?"),
                },
            ],
            stream: true,
            max_tokens: Some(1024),
            temperature: Some(0.7),
            top_p: None,
        };

        let body = provider.build_request_body(&request).unwrap();

        // system 应该被提取到独立字段
        assert!(body.systemInstruction.is_some());

        // 消息列表应该只有 3 条（不含 system）
        assert_eq!(body.contents.len(), 3);

        // 验证角色转换
        assert_eq!(body.contents[0].role, "user"); // 原始 user
        assert_eq!(body.contents[1].role, "model"); // assistant -> model
        assert_eq!(body.contents[2].role, "user"); // 原始 user
    }
}
