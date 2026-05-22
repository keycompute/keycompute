//! Google Gemini API 协议类型
//!
//! Gemini API 的请求/响应结构定义
//! 文档: https://ai.google.dev/api/generatecontent

use serde::{Deserialize, Serialize};

/// Gemini 默认 API 端点
pub const GEMINI_DEFAULT_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Gemini 支持的模型列表
pub const GEMINI_MODELS: &[&str] = &[
    "gemini-empty", // Gemini 示例空模型名称
];

/// Gemini Generate Content 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GeminiRequest {
    /// 对话内容
    pub contents: Vec<GeminiContent>,
    /// 系统指令
    #[serde(skip_serializing_if = "Option::is_none")]
    pub systemInstruction: Option<GeminiContent>,
    /// 生成配置
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generationConfig: Option<GenerationConfig>,
    /// 安全设置
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safetySettings: Option<Vec<SafetySetting>>,
}

/// Gemini 内容结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiContent {
    /// 角色: user, model
    pub role: String,
    /// 内容部分
    pub parts: Vec<GeminiPart>,
}

/// Gemini 内容部分
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GeminiPart {
    /// 文本内容
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 内联数据（图片等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inlineData: Option<InlineData>,
}

/// 内联数据（用于多模态）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct InlineData {
    /// MIME 类型
    pub mimeType: String,
    /// Base64 编码数据
    pub data: String,
}

/// 生成配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
#[derive(Default)]
pub struct GenerationConfig {
    /// 温度 (0-2)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Top P (0-1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topP: Option<f32>,
    /// Top K
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topK: Option<u32>,
    /// 最大输出 token 数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxOutputTokens: Option<u32>,
    /// 停止序列
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopSequences: Option<Vec<String>>,
}

/// 安全设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetySetting {
    /// 危害类别
    pub category: String,
    /// 阈值
    pub threshold: String,
}

/// Gemini Generate Content 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GeminiResponse {
    /// 候选结果
    pub candidates: Vec<GeminiCandidate>,
    /// 提示反馈
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promptFeedback: Option<PromptFeedback>,
    /// 用量元数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usageMetadata: Option<UsageMetadata>,
}

/// 候选结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GeminiCandidate {
    /// 内容
    pub content: GeminiContent,
    /// 完成原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finishReason: Option<String>,
    /// 安全评分
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safetyRatings: Option<Vec<SafetyRating>>,
}

/// 提示反馈
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct PromptFeedback {
    /// 阻止原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blockReason: Option<String>,
    /// 安全评分
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safetyRatings: Option<Vec<SafetyRating>>,
}

/// 安全评分
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyRating {
    /// 类别
    pub category: String,
    /// 概率
    pub probability: String,
}

/// 用量元数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[allow(non_snake_case)]
pub struct UsageMetadata {
    /// 提示 token 数
    #[serde(default)]
    pub promptTokenCount: u32,
    /// 候选 token 数
    #[serde(default)]
    pub candidatesTokenCount: u32,
    /// 总 token 数
    #[serde(default)]
    pub totalTokenCount: u32,
}

/// Gemini 流式响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GeminiStreamResponse {
    /// 候选结果
    pub candidates: Vec<GeminiCandidate>,
    /// 用量元数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usageMetadata: Option<UsageMetadata>,
}

impl GeminiRequest {
    /// 创建新的 Gemini 请求
    pub fn new() -> Self {
        Self {
            contents: Vec::new(),
            systemInstruction: None,
            generationConfig: None,
            safetySettings: None,
        }
    }

    /// 添加用户消息
    pub fn add_user_message(mut self, text: impl Into<String>) -> Self {
        self.contents.push(GeminiContent {
            role: "user".to_string(),
            parts: vec![GeminiPart {
                text: Some(text.into()),
                inlineData: None,
            }],
        });
        self
    }

    /// 添加模型消息（助手）
    pub fn add_model_message(mut self, text: impl Into<String>) -> Self {
        self.contents.push(GeminiContent {
            role: "model".to_string(),
            parts: vec![GeminiPart {
                text: Some(text.into()),
                inlineData: None,
            }],
        });
        self
    }

    /// 设置系统指令
    pub fn with_system_instruction(mut self, text: impl Into<String>) -> Self {
        self.systemInstruction = Some(GeminiContent {
            role: "user".to_string(),
            parts: vec![GeminiPart {
                text: Some(text.into()),
                inlineData: None,
            }],
        });
        self
    }

    /// 设置生成配置
    pub fn with_generation_config(mut self, config: GenerationConfig) -> Self {
        self.generationConfig = Some(config);
        self
    }

    /// 设置温度
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        let mut config = self.generationConfig.unwrap_or_default();
        config.temperature = Some(temperature);
        self.generationConfig = Some(config);
        self
    }

    /// 设置最大输出 tokens
    pub fn with_max_output_tokens(mut self, max_tokens: u32) -> Self {
        let mut config = self.generationConfig.unwrap_or_default();
        config.maxOutputTokens = Some(max_tokens);
        self.generationConfig = Some(config);
        self
    }
}

impl Default for GeminiRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiResponse {
    /// 提取文本内容
    pub fn extract_text(&self) -> String {
        self.candidates
            .first()
            .and_then(|candidate| candidate.content.parts.first())
            .and_then(|part| part.text.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_request_serialization() {
        let request = GeminiRequest::new()
            .with_system_instruction("You are helpful")
            .add_user_message("Hello")
            .with_temperature(0.7);

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("Hello"));
        assert!(json.contains("You are helpful"));
        assert!(json.contains("0.7"));
    }

    #[test]
    fn test_gemini_response_parsing() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "Hello! How can I help you?"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 20,
                "totalTokenCount": 30
            }
        }"#;

        let response: GeminiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.extract_text(), "Hello! How can I help you?");
        assert_eq!(response.usageMetadata.unwrap().totalTokenCount, 30);
    }

    #[test]
    fn test_gemini_models_list() {
        assert!(GEMINI_MODELS.contains(&"gemini-1.5-flash"));
        assert!(GEMINI_MODELS.contains(&"gemini-1.5-pro"));
        assert!(GEMINI_MODELS.contains(&"gemini-pro"));
    }
}
