//! 调试接口模块
//!
//! 提供路由调试、Provider 健康状态等调试功能（Admin）

use crate::client::ApiClient;
use crate::error::Result;
use serde::Deserialize;
use std::collections::HashMap;

/// 调试 API 客户端
#[derive(Debug, Clone)]
pub struct DebugApi {
    client: ApiClient,
}

impl DebugApi {
    /// 创建新的调试 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 获取路由调试信息
    pub async fn debug_routing(&self, token: &str) -> Result<RoutingDebugInfo> {
        self.client.get_json("/debug/routing", Some(token)).await
    }

    /// 获取 Provider 健康状态
    pub async fn get_provider_health(&self, token: &str) -> Result<ProviderHealthResponse> {
        self.client.get_json("/debug/providers", Some(token)).await
    }

    /// 获取网关状态
    pub async fn get_gateway_status(&self, token: &str) -> Result<GatewayStatus> {
        self.client
            .get_json("/debug/gateway/status", Some(token))
            .await
    }

    /// 获取网关统计
    pub async fn get_gateway_stats(&self, token: &str) -> Result<GatewayStats> {
        self.client
            .get_json("/debug/gateway/stats", Some(token))
            .await
    }

    /// 检查 Provider 健康
    pub async fn check_provider_health(&self, token: &str) -> Result<HealthCheckResponse> {
        self.client
            .post_json("/debug/gateway/health", &serde_json::json!({}), Some(token))
            .await
    }
}

/// 路由调试信息
#[derive(Debug, Clone, Deserialize)]
pub struct RoutingDebugInfo {
    pub routes: Vec<RouteInfo>,
}

/// 路由信息
#[derive(Debug, Clone, Deserialize)]
pub struct RouteInfo {
    pub path: String,
    pub method: String,
    pub handler: String,
}

/// Provider 健康响应
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderHealthResponse {
    pub providers: HashMap<String, ProviderHealth>,
}

/// Provider 健康状态
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderHealth {
    pub status: String,
    pub last_check: Option<String>,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
}

/// 网关状态
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayStatus {
    pub status: String,
    pub uptime_seconds: i64,
    pub version: String,
}

/// 网关统计
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayStats {
    pub total_requests: i64,
    pub successful_requests: i64,
    pub failed_requests: i64,
    pub average_latency_ms: f64,
    pub active_connections: i32,
}

/// 健康检查响应
#[derive(Debug, Clone, Deserialize)]
pub struct HealthCheckResponse {
    pub checked_providers: Vec<String>,
    pub healthy_providers: Vec<String>,
    pub unhealthy_providers: Vec<String>,
}
