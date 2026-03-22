//! Provider Adapter Trait
//!
//! 定义统一的 Provider 接口，被所有 Provider Adapter 实现。

use async_trait::async_trait;
use futures::Stream;
use keycompute_types::Result;
use std::pin::Pin;

pub mod request;
pub mod stream;

pub use request::{UpstreamMessage, UpstreamRequest};
pub use stream::StreamEvent;

/// Provider 适配器 trait
///
/// 所有 LLM Provider 必须实现此 trait，提供统一的上游调用接口
#[async_trait]
pub trait ProviderAdapter: Send + Sync + std::fmt::Debug {
    /// Provider 名称
    fn name(&self) -> &'static str;

    /// 支持的模型列表
    fn supported_models(&self) -> Vec<&'static str>;

    /// 检查是否支持指定模型
    fn supports_model(&self, model: &str) -> bool {
        self.supported_models().contains(&model)
    }

    /// 发起流式请求
    async fn stream_chat(&self, request: UpstreamRequest) -> Result<StreamBox>;

    /// 非流式请求（默认通过 stream 实现）
    async fn chat(&self, request: UpstreamRequest) -> Result<String> {
        let mut stream = self.stream_chat(request).await?;
        let mut content = String::new();

        use futures::StreamExt;
        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::Delta { content: delta, .. } => {
                    content.push_str(&delta);
                }
                StreamEvent::Done => break,
                StreamEvent::Error { message } => {
                    return Err(keycompute_types::KeyComputeError::ProviderError(message));
                }
                _ => {}
            }
        }

        Ok(content)
    }
}

/// 流返回类型
pub type StreamBox = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_event_serialization() {
        let event = StreamEvent::Delta {
            content: "Hello".to_string(),
            finish_reason: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Hello"));
    }
}
