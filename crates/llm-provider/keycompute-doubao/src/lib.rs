//! Doubao (豆包) Provider
//!
//! Doubao 是字节跳动旗下的 AI 模型，通过火山引擎提供 API 服务。
//! API 兼容 OpenAI 格式，因此复用 keycompute-openai 的协议和流处理。
//!
//! 主要差异：
//! - 默认端点: https://ark.cn-beijing.volces.com/api/v3/chat/completions
//! - 支持的模型: doubao-pro, doubao-lite, doubao-pro-32k, doubao-lite-32k 等

mod adapter;

pub use adapter::DoubaoProvider;
