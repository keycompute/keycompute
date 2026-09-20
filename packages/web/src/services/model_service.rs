//! 模型服务
//!
//! 获取系统支持的模型列表

use client_api::api::openai::ModelListResponse;
use client_api::error::Result;

use super::api_client::get_client;

/// 获取可用模型列表。模型发现始终带当前登录身份，避免匿名聚合或
/// 用静态占位模型掩盖后端依赖故障。
pub async fn list_models(
    mode: &str,
    protocol: &str,
    capability: &str,
    token: &str,
) -> Result<ModelListResponse> {
    let client = get_client();
    let path = models_path(mode, protocol, capability);
    client.get_json(&path, Some(token)).await
}

fn models_path(mode: &str, protocol: &str, capability: &str) -> String {
    match mode {
        "passthrough" => {
            format!("/pt/v1/models?protocol={protocol}&capability={capability}")
        }
        "node_dispatch" => {
            format!("/nt/v1/models?protocol={protocol}&capability={capability}")
        }
        _ => format!("/v1/models?mode=account_pool&protocol={protocol}&capability={capability}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_catalog_requests_the_responses_capability() {
        assert_eq!(
            models_path("account_pool", "openai", "responses"),
            "/v1/models?mode=account_pool&protocol=openai&capability=responses"
        );
        assert_eq!(
            models_path("passthrough", "openai", "chat_completions"),
            "/pt/v1/models?protocol=openai&capability=chat_completions"
        );
        assert_eq!(
            models_path("node_dispatch", "openai", "chat_completions"),
            "/nt/v1/models?protocol=openai&capability=chat_completions"
        );
    }

    #[test]
    fn node_model_paths_keep_raw_colon_ids_in_the_detail_url() {
        assert_eq!(
            format!("/nt/v1/models/{}", "gemma3:270m"),
            "/nt/v1/models/gemma3:270m"
        );
    }
}
