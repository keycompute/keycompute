//! 统一上游请求类型
//!
//! 定义发送到上游 Provider 的标准化请求格式

use serde::{Deserialize, Serialize};

/// 上游请求结构
///
/// 标准化的请求格式，各 Provider Adapter 负责转换为各自协议
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamRequest {
    /// 上游 API 端点
    pub endpoint: String,
    /// 上游 API Key
    pub api_key: String,
    /// 模型名称
    pub model: String,
    /// 消息列表
    pub messages: Vec<UpstreamMessage>,
    /// 是否流式输出
    pub stream: bool,
    /// 最大 token 数（可选）
    pub max_tokens: Option<u32>,
    /// 温度参数（可选）
    pub temperature: Option<f32>,
    /// Top P 参数（可选）
    pub top_p: Option<f32>,
}

impl UpstreamRequest {
    /// 创建新的上游请求
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            model: model.into(),
            messages: Vec::new(),
            stream: true,
            max_tokens: None,
            temperature: None,
            top_p: None,
        }
    }

    /// 添加消息
    pub fn with_message(mut self, role: impl Into<String>, content: impl Into<String>) -> Self {
        self.messages.push(UpstreamMessage {
            role: role.into(),
            content: content.into(),
        });
        self
    }

    /// 设置流式输出
    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// 设置最大 token 数
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// 设置温度参数
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }
}

/// 上游消息结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamMessage {
    /// 角色：system / user / assistant
    pub role: String,
    /// 消息内容
    pub content: String,
}

impl UpstreamMessage {
    /// 创建系统消息
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    /// 创建用户消息
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }

    /// 创建助手消息
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upstream_request_builder() {
        let request = UpstreamRequest::new(
            "https://api.openai.com/v1/chat/completions",
            "sk-test",
            "gpt-4o",
        )
        .with_message("system", "You are a helpful assistant")
        .with_message("user", "Hello")
        .with_stream(true)
        .with_max_tokens(1000)
        .with_temperature(0.7);

        assert_eq!(request.model, "gpt-4o");
        assert_eq!(request.messages.len(), 2);
        assert!(request.stream);
        assert_eq!(request.max_tokens, Some(1000));
    }

    #[test]
    fn test_upstream_message_helpers() {
        let sys = UpstreamMessage::system("System prompt");
        let user = UpstreamMessage::user("User input");
        let assistant = UpstreamMessage::assistant("Assistant response");

        assert_eq!(sys.role, "system");
        assert_eq!(user.role, "user");
        assert_eq!(assistant.role, "assistant");
    }
}
