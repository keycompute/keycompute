//! Ollama Provider Adapter
//!
//! 本地模型支持，通过 Ollama 运行本地 LLM。
//!
//! ## 支持的模型
//! - Llama 3.x 系列 (llama3.2, llama3.1)
//! - Mistral / Mixtral 系列
//! - Gemma 系列
//! - Qwen 系列
//! - DeepSeek 系列
//! - Phi-3 系列
//! - CodeLlama, StarCoder2 等
//!
//! ## API 端点
//! 默认: http://localhost:11434/api/chat
//!
//! ## 认证
//! Ollama 通常不需要认证（本地服务）

pub mod adapter;
pub mod protocol;
pub mod stream;

pub use adapter::{OLLAMA_DEFAULT_ENDPOINT, OLLAMA_MODELS, OllamaProvider};
pub use protocol::{
    OllamaError, OllamaMessage, OllamaOptions, OllamaRequest, OllamaResponse, OllamaStreamResponse,
};
pub use stream::parse_ollama_stream;

#[cfg(test)]
mod tests {
    use super::*;
    use keycompute_provider_trait::ProviderAdapter;

    #[test]
    fn test_ollama_provider_exports() {
        let provider = adapter::OllamaProvider::new();
        assert_eq!(provider.name(), "ollama");
        assert!(!adapter::OLLAMA_MODELS.is_empty());
    }
}
