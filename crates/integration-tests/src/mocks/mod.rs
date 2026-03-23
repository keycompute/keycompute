//! 测试模拟对象
//!
//! 提供各 crate 的 mock 实现，用于隔离测试

use uuid::Uuid;

/// 模拟 Provider Adapter
pub mod provider;

/// 模拟 HTTP Transport
pub mod http_transport;

/// 模拟数据库
pub mod database;

/// 模拟上游 Provider 响应
#[derive(Debug, Clone)]
pub struct MockProviderResponse {
    pub provider_name: String,
    pub model: String,
    pub chunks: Vec<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl MockProviderResponse {
    pub fn new(provider: &str, model: &str) -> Self {
        Self {
            provider_name: provider.to_string(),
            model: model.to_string(),
            chunks: vec![
                "Hello".to_string(),
                " from".to_string(),
                " mock".to_string(),
                " provider".to_string(),
            ],
            input_tokens: 10,
            output_tokens: 4,
        }
    }

    pub fn with_chunks(mut self, chunks: Vec<String>) -> Self {
        let len = chunks.len();
        self.chunks = chunks;
        self.output_tokens = len as u32;
        self
    }

    pub fn with_tokens(mut self, input: u32, output: u32) -> Self {
        self.input_tokens = input;
        self.output_tokens = output;
        self
    }
}

/// 模拟执行上下文
#[derive(Debug, Clone)]
pub struct MockExecutionContext {
    pub request_id: Uuid,
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub api_key_id: Uuid,
    pub model: String,
    pub provider: String,
    pub account_id: Uuid,
}

impl MockExecutionContext {
    pub fn new() -> Self {
        Self {
            request_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            api_key_id: Uuid::new_v4(),
            model: "gpt-4o".to_string(),
            provider: "openai".to_string(),
            account_id: Uuid::new_v4(),
        }
    }
}

impl Default for MockExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}
