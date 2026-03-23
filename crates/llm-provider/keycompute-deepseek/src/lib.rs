//! DeepSeek Provider Adapter
//!
//! DeepSeek API 的 Provider 适配器实现。
//! DeepSeek API 与 OpenAI API 高度兼容，因此复用 OpenAI 的协议层。
//!
//! # 支持的模型
//! - `deepseek-chat` - 通用对话模型
//! - `deepseek-coder` - 代码专用模型
//! - `deepseek-reasoner` - 推理增强模型
//!
//! # 使用示例
//! ```rust
//! use keycompute_deepseek::DeepSeekProvider;
//! use keycompute_provider_trait::ProviderAdapter;
//!
//! let provider = DeepSeekProvider::new();
//! assert_eq!(provider.name(), "deepseek");
//! assert!(provider.supports_model("deepseek-chat"));
//! ```

pub mod adapter;

pub use adapter::{DeepSeekProvider, DEEPSEEK_DEFAULT_ENDPOINT, DEEPSEEK_MODELS};

// 复用 OpenAI 的协议类型，DeepSeek API 与 OpenAI API 完全兼容
pub use keycompute_openai::{OpenAIRequest, OpenAIResponse, OpenAIStreamResponse};
