//! 模型服务
//!
//! 获取系统支持的模型列表

use client_api::ClientConfig;
use client_api::error::Result;
use client_api::{OpenAiClient, api::openai::ModelListResponse};

use super::api_client::get_client;

/// 获取可用模型列表（无需认证）
///
/// `protocol` 为入口协议（openai / anthropic）：后端按协议过滤账号
/// 声明的模型，与入口协议隔离保持一致（OpenAI 兼容入口只列出
/// openai 协议模型，Anthropic 示例需显式传 anthropic）。
/// 注：protocol 取值受限于视图层固定值（openai/anthropic），直接拼接
/// query 无编码风险；web 为 WASM 包不引入 urlencoding 依赖。
pub async fn list_models(protocol: &str, capability: Option<&str>) -> Result<ModelListResponse> {
    let client = get_client();
    let base_url = client.config().base_url.clone();
    let openai_client = OpenAiClient::new(ClientConfig::new(base_url))?;
    // 使用空字符串作为 API key，因为后端允许匿名访问 /v1/models
    let path = models_path(protocol, capability);
    openai_client.get_json(&path, "").await
}

fn models_path(protocol: &str, capability: Option<&str>) -> String {
    capability.map_or_else(
        || format!("/v1/models?protocol={protocol}"),
        |capability| format!("/v1/models?protocol={protocol}&capability={capability}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_catalog_requests_the_responses_capability() {
        assert_eq!(
            models_path("openai", Some("responses")),
            "/v1/models?protocol=openai&capability=responses"
        );
        assert_eq!(
            models_path("openai", Some("chat_completions")),
            "/v1/models?protocol=openai&capability=chat_completions"
        );
    }
}
