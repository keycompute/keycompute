//! 租户管理模块
//!
//! 处理租户列表查询（Admin）

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

/// 租户 API 客户端
#[derive(Debug, Clone)]
pub struct TenantApi {
    client: ApiClient,
}

impl TenantApi {
    /// 创建新的租户 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 获取租户列表（Admin）
    pub async fn list_tenants(
        &self,
        params: Option<&TenantQueryParams>,
        token: &str,
    ) -> Result<Vec<TenantInfo>> {
        let path = if let Some(p) = params {
            format!("/api/v1/tenants?{}", p.to_query_string())
        } else {
            "/api/v1/tenants".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }
}

/// 租户查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct TenantQueryParams {
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

impl TenantQueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limit(mut self, limit: i32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_offset(mut self, offset: i32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(limit) = self.limit {
            params.push(format!("limit={}", limit));
        }
        if let Some(offset) = self.offset {
            params.push(format!("offset={}", offset));
        }
        params.join("&")
    }
}

/// 租户信息
#[derive(Debug, Clone, Deserialize)]
pub struct TenantInfo {
    pub id: String,
    pub name: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}
