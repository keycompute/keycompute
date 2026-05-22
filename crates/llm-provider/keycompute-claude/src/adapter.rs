//! Claude Provider Adapter 实现
//!
//! 实现 ProviderAdapter trait，提供 Anthropic Claude API 的调用能力
//!
//! Claude Messages API 与 OpenAI API 的主要差异：
//! - 端点: https://api.anthropic.com/v1/messages
//! - 认证: x-api-key 头部（而非 Authorization: Bearer）
//! - 请求结构: messages 数组不包含 system 角色，system 是独立字段
//! - 响应结构: content 是数组而非单一字符串
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
use keycompute_types::{KeyComputeError, Result};
use serde_json;

use crate::protocol::{ClaudeContent, ClaudeMessage, ClaudeRequest, ClaudeResponse};
use crate::stream::parse_claude_stream;

/// Claude 默认 API 端点
pub const CLAUDE_DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// Claude API 版本
pub const CLAUDE_API_VERSION: &str = "2023-06-01";

/// Claude 支持的模型列表
pub const CLAUDE_MODELS: &[&str] = &[
    "claude-empty", // Claude 示例空模型名称
];

/// Claude Provider 适配器
#[derive(Debug, Clone)]
pub struct ClaudeProvider;

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeProvider {
    /// 创建新的 Claude Provider
    pub fn new() -> Self {
        Self
    }

    /// 获取提供者名称
    pub fn name(&self) -> &'static str {
        "claude"
    }

    /// 构建 Claude 请求体
    ///
    /// 将标准化的 UpstreamRequest 转换为 Claude Messages API 格式
    fn build_request_body(&self, request: &UpstreamRequest) -> ClaudeRequest {
        // 分离 system 消息和普通消息
        let mut system_content = None;
        let mut messages = Vec::new();

        for msg in &request.messages {
            if msg.role == "system" {
                // Claude 的 system 是独立字段，不是消息角色
                system_content = Some(msg.content.clone());
            } else {
                // 转换角色: OpenAI 的 "assistant" -> Claude 的 "assistant"
                // OpenAI 的 "user" -> Claude 的 "user"
                let role = if msg.role == "assistant" {
                    "assistant"
                } else {
                    "user"
                };

                messages.push(ClaudeMessage {
                    role: role.to_string(),
                    content: ClaudeContent::Text(msg.content.clone()),
                });
            }
        }

        // 默认 max_tokens（Claude 要求必须提供）
        let max_tokens = request.max_tokens.unwrap_or(4096);

        ClaudeRequest {
            model: request.model.clone(),
            max_tokens,
            messages,
            system: system_content,
            stream: Some(request.stream),
            temperature: request.temperature,
            top_p: request.top_p,
            stop_sequences: None,
            metadata: None,
        }
    }

    /// 获取实际请求端点
    fn get_endpoint(&self, request: &UpstreamRequest) -> String {
        if request.endpoint.is_empty() {
            CLAUDE_DEFAULT_ENDPOINT.to_string()
        } else {
            request.endpoint.clone()
        }
    }

    /// 构建 Claude API 请求头
    fn build_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), api_key.to_string()),
            (
                "anthropic-version".to_string(),
                CLAUDE_API_VERSION.to_string(),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
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

        let headers = self.build_headers(request.upstream_api_key.expose());

        let response_text = transport.post_json(&endpoint, headers, body_json).await?;

        let claude_response: ClaudeResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                KeyComputeError::ProviderError(format!("Failed to parse Claude response: {}", e))
            })?;

        // 提取文本内容
        let content = claude_response.extract_text();
        Ok(content)
    }

    /// 执行流式请求
    async fn stream_chat_internal(
        &self,
        transport: &dyn HttpTransport,
        request: UpstreamRequest,
    ) -> Result<StreamBox> {
        let mut body = self.build_request_body(&request);
        let endpoint = self.get_endpoint(&request);

        // 确保启用流式输出
        body.stream = Some(true);

        let body_json = serde_json::to_string(&body).map_err(|e| {
            KeyComputeError::ProviderError(format!("Failed to serialize request: {}", e))
        })?;

        let mut headers = self.build_headers(request.upstream_api_key.expose());
        headers.push(("Accept".to_string(), "text/event-stream".to_string()));

        let byte_stream: ByteStream = transport.post_stream(&endpoint, headers, body_json).await?;

        // 转换为标准化的 StreamEvent 流
        Ok(parse_claude_stream(byte_stream))
    }
}

#[async_trait]
impl ProviderAdapter for ClaudeProvider {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn supported_models(&self) -> Vec<&'static str> {
        CLAUDE_MODELS.to_vec()
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
    fn test_claude_provider_name() {
        let provider = ClaudeProvider::new();
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn test_claude_supported_models() {
        let provider = ClaudeProvider::new();
        let models = provider.supported_models();
        // CLAUDE_MODELS 已替换为示例空模型，仅保留 claude-empty 占位
        assert!(models.contains(&"claude-empty"));
    }

    #[test]
    fn test_claude_supports_model() {
        let provider = ClaudeProvider::new();
        assert!(provider.supports_model("claude-empty"));
        assert!(!provider.supports_model("gpt-4o"));
    }

    #[test]
    fn test_default_endpoint() {
        assert_eq!(
            CLAUDE_DEFAULT_ENDPOINT,
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_build_request_body() {
        let provider = ClaudeProvider::new();
        let request = UpstreamRequest::new(
            "https://api.anthropic.com/v1/messages",
            "sk-test",
            "claude-3-5-sonnet-20241022",
        )
        .with_message("system", "You are helpful")
        .with_message("user", "Hello")
        .with_stream(true)
        .with_temperature(0.7);

        let body = provider.build_request_body(&request);

        assert_eq!(body.model, "claude-3-5-sonnet-20241022");
        assert_eq!(body.system, Some("You are helpful".to_string()));
        assert_eq!(body.messages.len(), 1); // 只有 user 消息
        assert_eq!(body.stream, Some(true));
        assert_eq!(body.temperature, Some(0.7));
        assert_eq!(body.max_tokens, 4096); // 默认值
    }

    #[test]
    fn test_build_request_body_with_max_tokens() {
        let provider = ClaudeProvider::new();
        let request = UpstreamRequest::new(
            "https://api.anthropic.com/v1/messages",
            "sk-test",
            "claude-3-5-sonnet-20241022",
        )
        .with_message("user", "Hello")
        .with_max_tokens(1024);

        let body = provider.build_request_body(&request);
        assert_eq!(body.max_tokens, 1024);
    }

    #[test]
    fn test_get_endpoint_default() {
        let provider = ClaudeProvider::new();
        let request = UpstreamRequest::new(
            "", // 空端点
            "sk-test",
            "claude-3-5-sonnet-20241022",
        );

        assert_eq!(provider.get_endpoint(&request), CLAUDE_DEFAULT_ENDPOINT);
    }

    #[test]
    fn test_get_endpoint_custom() {
        let provider = ClaudeProvider::new();
        let custom_endpoint = "https://custom.anthropic.com/v1/messages";
        let request =
            UpstreamRequest::new(custom_endpoint, "sk-test", "claude-3-5-sonnet-20241022");

        assert_eq!(provider.get_endpoint(&request), custom_endpoint);
    }

    #[test]
    fn test_build_headers() {
        let provider = ClaudeProvider::new();
        let headers = provider.build_headers("sk-test-key");

        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "x-api-key" && v == "sk-test-key")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "anthropic-version" && v == CLAUDE_API_VERSION)
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "Content-Type" && v == "application/json")
        );
    }

    #[test]
    fn test_build_request_body_converts_roles() {
        let provider = ClaudeProvider::new();
        let request = UpstreamRequest {
            endpoint: "https://api.anthropic.com/v1/messages".to_string(),
            upstream_api_key: "sk-test".into(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            messages: vec![
                UpstreamMessage {
                    role: "system".to_string(),
                    content: "You are helpful".to_string(),
                },
                UpstreamMessage {
                    role: "user".to_string(),
                    content: "Hello".to_string(),
                },
                UpstreamMessage {
                    role: "assistant".to_string(),
                    content: "Hi there!".to_string(),
                },
                UpstreamMessage {
                    role: "user".to_string(),
                    content: "How are you?".to_string(),
                },
            ],
            stream: true,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
        };

        let body = provider.build_request_body(&request);

        // system 应该被提取到独立字段
        assert_eq!(body.system, Some("You are helpful".to_string()));

        // 消息列表应该只有 3 条（不含 system）
        assert_eq!(body.messages.len(), 3);

        // 验证角色转换
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(body.messages[1].role, "assistant");
        assert_eq!(body.messages[2].role, "user");
    }
}
