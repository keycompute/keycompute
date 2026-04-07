//! Provider 定义模块
//!
//! 统一定义所有可用的 LLM Provider，作为系统的单一数据源。
//! Gateway 和 RoutingEngine 都从这里获取 Provider 列表，确保一致性。

use keycompute_provider_trait::ProviderAdapter;
use std::sync::Arc;

/// Provider 定义
pub struct ProviderDefinition {
    /// Provider 名称（用于路由和日志）
    pub name: &'static str,
    /// Provider 描述
    pub description: &'static str,
    /// 创建 Provider Adapter 的函数
    pub create_adapter: fn() -> Arc<dyn ProviderAdapter>,
}

/// 所有可用的 Provider 列表
///
/// 这是系统的单一数据源，Gateway 和 RoutingEngine 都从这里获取 Provider 列表。
/// 添加新 Provider 时只需修改此处，无需同步修改其他模块。
pub const AVAILABLE_PROVIDERS: &[ProviderDefinition] = &[
    ProviderDefinition {
        name: "openai",
        description: "OpenAI GPT Models",
        create_adapter: || Arc::new(keycompute_openai::OpenAIProvider::new()),
    },
    ProviderDefinition {
        name: "deepseek",
        description: "DeepSeek Models",
        create_adapter: || Arc::new(keycompute_deepseek::DeepSeekProvider::new()),
    },
    ProviderDefinition {
        name: "vllm",
        description: "vLLM Self-hosted Models",
        create_adapter: || Arc::new(keycompute_vllm::VllmProvider::new()),
    },
    ProviderDefinition {
        name: "claude",
        description: "Anthropic Claude Models",
        create_adapter: || Arc::new(keycompute_claude::ClaudeProvider::new()),
    },
    ProviderDefinition {
        name: "ollama",
        description: "Ollama Local Models",
        create_adapter: || Arc::new(keycompute_ollama::OllamaProvider::new()),
    },
    ProviderDefinition {
        name: "gemini",
        description: "Google Gemini Models",
        create_adapter: || Arc::new(keycompute_gemini::GeminiProvider::new()),
    },
];

/// 获取所有 Provider 名称列表
///
/// 用于 RoutingEngine 初始化
pub fn get_provider_names() -> Vec<String> {
    AVAILABLE_PROVIDERS
        .iter()
        .map(|p| p.name.to_string())
        .collect()
}

/// 获取所有 Provider Adapter
///
/// 用于 GatewayBuilder 初始化
pub fn get_provider_adapters() -> Vec<(String, Arc<dyn ProviderAdapter>)> {
    AVAILABLE_PROVIDERS
        .iter()
        .map(|p| {
            let adapter = (p.create_adapter)();
            (p.name.to_string(), adapter)
        })
        .collect()
}

/// 检查 Provider 是否可用
pub fn is_provider_available(name: &str) -> bool {
    AVAILABLE_PROVIDERS.iter().any(|p| p.name == name)
}

/// 获取 Provider 定义
pub fn get_provider_definition(name: &str) -> Option<&'static ProviderDefinition> {
    AVAILABLE_PROVIDERS.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_names_count() {
        let names = get_provider_names();
        assert_eq!(names.len(), AVAILABLE_PROVIDERS.len());
    }

    #[test]
    fn test_is_provider_available() {
        assert!(is_provider_available("openai"));
        assert!(is_provider_available("claude"));
        assert!(!is_provider_available("unknown"));
    }

    #[test]
    fn test_get_provider_adapters() {
        let adapters = get_provider_adapters();
        assert_eq!(adapters.len(), AVAILABLE_PROVIDERS.len());

        // 验证每个 adapter 都能创建
        for (name, _) in adapters {
            assert!(is_provider_available(&name));
        }
    }
}
