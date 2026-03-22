//! OpenAI Provider Adapter
//!
//! OpenAI API 的 Provider 适配器实现，作为参考实现和 OpenAI-compatible Provider 的基类。

pub mod adapter;
pub mod protocol;
pub mod stream;

pub use adapter::OpenAIProvider;
pub use protocol::{OpenAIRequest, OpenAIResponse, OpenAIStreamResponse};
