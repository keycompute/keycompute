//! 租户管理模块
//!
//! 处理租户列表查询、创建、更新、删除（Admin）

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

const COMPAT_LIST_PAGE_SIZE: u32 = 100;

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
        if params.is_some_and(TenantQueryParams::has_explicit_pagination) {
            return Ok(self.list_tenants_page(params, token).await?.tenants);
        }
        self.collect_tenant_pages(params, token).await
    }

    /// 获取租户分页列表（Admin）。
    pub async fn list_tenants_page(
        &self,
        params: Option<&TenantQueryParams>,
        token: &str,
    ) -> Result<TenantPage> {
        let path = if let Some(p) = params {
            format!("/api/v1/tenants?{}", p.to_query_string())
        } else {
            "/api/v1/tenants".to_string()
        };
        self.client.get_json(&path, Some(token)).await
    }

    /// 获取全部租户，供必须展示完整租户选项的管理表单使用。
    pub async fn list_all_tenants(&self, token: &str) -> Result<Vec<TenantInfo>> {
        self.collect_tenant_pages(None, token).await
    }

    async fn collect_tenant_pages(
        &self,
        params: Option<&TenantQueryParams>,
        token: &str,
    ) -> Result<Vec<TenantInfo>> {
        let mut params = params.cloned().unwrap_or_default();
        params.page_size = Some(COMPAT_LIST_PAGE_SIZE);
        params.limit = None;
        params.offset = None;

        let mut tenants = Vec::new();
        let mut page = 1u32;
        loop {
            params.page = Some(page);
            let response = self.list_tenants_page(Some(&params), token).await?;
            tenants.extend(response.tenants);
            if response.total_pages == 0 || page >= response.total_pages {
                break;
            }
            page += 1;
        }
        Ok(tenants)
    }

    /// 创建租户（Admin）
    pub async fn create_tenant(
        &self,
        req: &CreateTenantRequest,
        token: &str,
    ) -> Result<TenantInfo> {
        self.client
            .post_json("/api/v1/tenants", req, Some(token))
            .await
    }

    /// 更新租户信息（Admin）
    pub async fn update_tenant(
        &self,
        tenant_id: &str,
        req: &UpdateTenantRequest,
        token: &str,
    ) -> Result<TenantInfo> {
        let path = format!("/api/v1/tenants/{}", tenant_id);
        self.client.put_json(&path, req, Some(token)).await
    }

    /// 删除租户（Admin）
    pub async fn delete_tenant(
        &self,
        tenant_id: &str,
        token: &str,
    ) -> Result<crate::api::common::MessageResponse> {
        let path = format!("/api/v1/tenants/{}", tenant_id);
        self.client.delete_json(&path, Some(token)).await
    }
}

/// 创建租户请求
#[derive(Debug, Clone, Serialize)]
pub struct CreateTenantRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl CreateTenantRequest {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: None,
        }
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }
}

/// 更新租户请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateTenantRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl UpdateTenantRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }
}

/// 租户查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct TenantQueryParams {
    pub search: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
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

    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    pub fn with_page(mut self, page: u32) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    fn has_explicit_pagination(&self) -> bool {
        self.page.is_some()
            || self.page_size.is_some()
            || self.limit.is_some()
            || self.offset.is_some()
    }

    pub fn with_offset(mut self, offset: i32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(ref search) = self.search {
            params.push(format!(
                "search={}",
                crate::api::common::encode_query_value(search)
            ));
        }
        if let Some(page) = self.page {
            params.push(format!("page={page}"));
        }
        if let Some(page_size) = self.page_size {
            params.push(format!("page_size={page_size}"));
        }
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
    pub description: Option<String>,
    pub user_count: i64,
    pub is_active: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TenantPage {
    pub tenants: Vec<TenantInfo>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub total_pages: u32,
}

#[cfg(test)]
mod tests {
    use super::TenantQueryParams;

    #[test]
    fn tenant_query_serializes_server_side_search_and_pagination() {
        let query = TenantQueryParams::new()
            .with_search("研发 租户")
            .with_page(2)
            .with_page_size(20)
            .to_query_string();
        assert_eq!(
            query,
            "search=%E7%A0%94%E5%8F%91%20%E7%A7%9F%E6%88%B7&page=2&page_size=20"
        );
    }
}
