//! 健康检查模块
//!
//! 检查服务健康状态

use crate::client::ApiClient;
use crate::error::Result;
use serde::Deserialize;

/// 健康检查 API 客户端
#[derive(Debug, Clone)]
pub struct HealthApi {
    client: ApiClient,
}

impl HealthApi {
    /// 创建新的健康检查 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 检查服务健康状态
    pub async fn health_check(&self) -> Result<HealthResponse> {
        self.client.get_json("/health", None).await
    }
}

/// 健康检查响应
#[derive(Debug, Clone, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: Option<String>,
    pub timestamp: Option<String>,
}

impl HealthResponse {
    /// 检查是否健康
    pub fn is_healthy(&self) -> bool {
        self.status == "healthy" || self.status == "ok"
    }
}
