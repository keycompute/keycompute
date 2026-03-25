//! Ollama API 协议类型
//!
//! Ollama Chat API 的请求/响应结构定义
//! 文档: https://github.com/ollama/ollama/blob/main/docs/api.md
//!
//! Ollama 支持两种 API 格式：
//! 1. 原生格式: POST /api/chat
//! 2. OpenAI 兼容格式: POST /v1/chat/completions
//!
//! 本模块实现原生格式，同时支持 OpenAI 兼容端点

use serde::{Deserialize, Serialize};

/// Ollama Chat API 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaRequest {
    /// 模型名称
    pub model: String,
    /// 消息列表
    pub messages: Vec<OllamaMessage>,
    /// 是否流式输出
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// 格式指定（如 "json"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// 生成选项
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<OllamaOptions>,
    /// 系统提示词（覆盖模型默认）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
}

/// Ollama 消息结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaMessage {
    /// 角色: system, user, assistant
    pub role: String,
    /// 消息内容
    pub content: String,
    /// 图片（多模态，base64）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
}

/// Ollama 生成选项
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OllamaOptions {
    /// 温度参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Top P 参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Top K 参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    /// 最大生成 token 数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<i32>,
    /// 停止序列
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// 种子
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
}

/// Ollama Chat API 响应（非流式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaResponse {
    /// 模型名称
    pub model: String,
    /// 创建时间
    pub created_at: String,
    /// 消息内容
    pub message: OllamaMessage,
    /// 是否完成
    pub done: bool,
    /// 用量统计（仅在 done=true 时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_eval_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_count: Option<u32>,
    /// 总耗时
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_duration: Option<u64>,
    /// 加载耗时
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_duration: Option<u64>,
    /// 提示词评估耗时
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_eval_duration: Option<u64>,
    /// 生成耗时
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_duration: Option<u64>,
}

/// Ollama 流式响应（每行一个 JSON）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaStreamResponse {
    /// 模型名称
    pub model: String,
    /// 创建时间
    pub created_at: String,
    /// 消息内容（增量）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<OllamaMessage>,
    /// 是否完成
    pub done: bool,
    /// 用量统计（仅在 done=true 时存在）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_eval_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_count: Option<u32>,
    /// 总耗时（纳秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_duration: Option<u64>,
    /// 提示词评估耗时（纳秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_eval_duration: Option<u64>,
    /// 生成耗时（纳秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eval_duration: Option<u64>,
}

/// Ollama 错误响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaError {
    /// 错误消息
    pub error: String,
}

impl OllamaRequest {
    /// 创建新的请求
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            stream: None,
            format: None,
            options: None,
            system: None,
        }
    }

    /// 添加消息
    pub fn add_message(mut self, role: impl Into<String>, content: impl Into<String>) -> Self {
        self.messages.push(OllamaMessage {
            role: role.into(),
            content: content.into(),
            images: None,
        });
        self
    }

    /// 设置系统提示词
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// 设置流式输出
    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = Some(stream);
        self
    }

    /// 设置生成选项
    pub fn with_options(mut self, options: OllamaOptions) -> Self {
        self.options = Some(options);
        self
    }
}

impl OllamaOptions {
    /// 创建新的选项
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置温度
    pub fn temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// 设置 top_p
    pub fn top_p(mut self, top_p: f32) -> Self {
        self.top_p = Some(top_p);
        self
    }

    /// 设置最大生成 token 数
    pub fn num_predict(mut self, num: i32) -> Self {
        self.num_predict = Some(num);
        self
    }
}

impl OllamaMessage {
    /// 创建用户消息
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
            images: None,
        }
    }

    /// 创建助手消息
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            images: None,
        }
    }

    /// 创建系统消息
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
            images: None,
        }
    }
}

impl OllamaResponse {
    /// 提取文本内容
    pub fn extract_text(&self) -> &str {
        &self.message.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_request_serialization() {
        let request = OllamaRequest::new("llama2")
            .add_message("user", "Hello")
            .with_stream(true);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("llama2"));
        assert!(json.contains("Hello"));
        assert!(json.contains("true"));
    }

    #[test]
    fn test_ollama_request_with_system() {
        let request = OllamaRequest::new("llama2")
            .with_system("You are helpful")
            .add_message("user", "Hello");

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("You are helpful"));
    }

    #[test]
    fn test_ollama_response_parsing() {
        let json = r#"{
            "model": "llama2",
            "created_at": "2023-08-04T08:52:19.385406455Z",
            "message": {"role": "assistant", "content": "Hello there!"},
            "done": true,
            "prompt_eval_count": 10,
            "eval_count": 5
        }"#;

        let response: OllamaResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.model, "llama2");
        assert_eq!(response.extract_text(), "Hello there!");
        assert_eq!(response.prompt_eval_count, Some(10));
        assert_eq!(response.eval_count, Some(5));
    }

    #[test]
    fn test_ollama_stream_response_parsing() {
        // 流式增量响应
        let json = r#"{
            "model": "llama2",
            "created_at": "2023-08-04T08:52:19.385406455Z",
            "message": {"role": "assistant", "content": "Hello"},
            "done": false
        }"#;

        let response: OllamaStreamResponse = serde_json::from_str(json).unwrap();
        assert!(!response.done);
        assert!(response.message.is_some());
        assert_eq!(response.message.unwrap().content, "Hello");

        // 完成响应
        let json = r#"{
            "model": "llama2",
            "created_at": "2023-08-04T08:52:19.385406455Z",
            "done": true,
            "prompt_eval_count": 10,
            "eval_count": 5,
            "total_duration": 1000000000
        }"#;

        let response: OllamaStreamResponse = serde_json::from_str(json).unwrap();
        assert!(response.done);
        assert_eq!(response.eval_count, Some(5));
    }

    #[test]
    fn test_ollama_options() {
        let options = OllamaOptions::new()
            .temperature(0.7)
            .top_p(0.9)
            .num_predict(100);

        let json = serde_json::to_string(&options).unwrap();
        assert!(json.contains("\"temperature\":0.7"));
        assert!(json.contains("\"top_p\":0.9"));
        assert!(json.contains("\"num_predict\":100"));
    }
}
