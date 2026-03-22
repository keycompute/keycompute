//! 请求标准化
//!
//! 将内部请求标准化为 Provider 格式。

use keycompute_types::RequestContext;
use keycompute_provider_trait::UpstreamRequest;

/// 请求标准化器
#[derive(Debug, Clone, Default)]
pub struct RequestNormalizer;

impl RequestNormalizer {
    /// 创建新的标准化器
    pub fn new() -> Self {
        Self {}
    }

    /// 标准化请求
    ///
    /// 将内部 RequestContext 转换为上游 UpstreamRequest
    pub fn normalize(&self, ctx: &RequestContext, endpoint: &str, api_key: &str) -> UpstreamRequest {
        let messages: Vec<keycompute_provider_trait::UpstreamMessage> = ctx
            .messages
            .iter()
            .map(|m| keycompute_provider_trait::UpstreamMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        UpstreamRequest {
            endpoint: endpoint.to_string(),
            api_key: api_key.to_string(),
            model: ctx.model.clone(),
            messages,
            stream: ctx.stream,
            max_tokens: None,
            temperature: None,
            top_p: None,
        }
    }

    /// 标准化模型名称
    ///
    /// 处理不同 Provider 的模型名称映射
    pub fn normalize_model(&self, model: &str, provider: &str) -> String {
        match provider {
            "openai" => model.to_string(),
            "claude" => {
                // 映射 OpenAI 模型名到 Claude 模型名
                match model {
                    "gpt-4o" => "claude-3-opus-20240229".to_string(),
                    "gpt-4o-mini" => "claude-3-haiku-20240307".to_string(),
                    _ => "claude-3-sonnet-20240229".to_string(),
                }
            }
            _ => model.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keycompute_types::{Message, PricingSnapshot, UsageAccumulator};
    use rust_decimal::Decimal;
    use uuid::Uuid;

    fn create_test_context() -> RequestContext {
        RequestContext {
            request_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            api_key_id: Uuid::new_v4(),
            model: "gpt-4o".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
            stream: true,
            pricing_snapshot: PricingSnapshot {
                model_name: "gpt-4o".to_string(),
                currency: "CNY".to_string(),
                input_price_per_1k: Decimal::from(1),
                output_price_per_1k: Decimal::from(2),
            },
            usage: UsageAccumulator::default(),
            started_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_request_normalizer_new() {
        let normalizer = RequestNormalizer::new();
        let ctx = create_test_context();
        let request = normalizer.normalize(&ctx, "https://api.example.com", "key");

        assert_eq!(request.model, "gpt-4o");
        assert_eq!(request.endpoint, "https://api.example.com");
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn test_normalize_model_openai() {
        let normalizer = RequestNormalizer::new();
        assert_eq!(normalizer.normalize_model("gpt-4o", "openai"), "gpt-4o");
    }

    #[test]
    fn test_normalize_model_claude() {
        let normalizer = RequestNormalizer::new();
        let model = normalizer.normalize_model("gpt-4o", "claude");
        assert!(model.starts_with("claude-3"));
    }
}
