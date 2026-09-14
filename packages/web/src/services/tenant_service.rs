use client_api::api::common::MessageResponse;
use client_api::error::Result;
use client_api::{
    TenantApi,
    api::tenant::{
        CreateTenantRequest, TenantInfo, TenantPage, TenantQueryParams, UpdateTenantRequest,
    },
};

use super::api_client::get_client;

#[allow(dead_code)]
pub async fn list(params: Option<TenantQueryParams>, token: &str) -> Result<Vec<TenantInfo>> {
    let client = get_client();
    TenantApi::new(&client)
        .list_tenants(params.as_ref(), token)
        .await
}

pub async fn list_page(params: TenantQueryParams, token: &str) -> Result<TenantPage> {
    let client = get_client();
    TenantApi::new(&client)
        .list_tenants_page(Some(&params), token)
        .await
}

pub async fn list_all(token: &str) -> Result<Vec<TenantInfo>> {
    let client = get_client();
    TenantApi::new(&client).list_all_tenants(token).await
}

/// 获取可供其它资源绑定的租户。非活跃租户仅在租户管理页展示，
/// 不应出现在账号、定价等资源的选择器中。
pub async fn list_active(token: &str) -> Result<Vec<TenantInfo>> {
    let tenants = list_all(token).await?;
    Ok(filter_active(tenants))
}

fn filter_active(tenants: Vec<TenantInfo>) -> Vec<TenantInfo> {
    tenants
        .into_iter()
        .filter(|tenant| tenant.is_active)
        .collect()
}

#[allow(dead_code)]
pub async fn create(req: CreateTenantRequest, token: &str) -> Result<TenantInfo> {
    let client = get_client();
    TenantApi::new(&client).create_tenant(&req, token).await
}

#[allow(dead_code)]
pub async fn update(tenant_id: &str, req: UpdateTenantRequest, token: &str) -> Result<TenantInfo> {
    let client = get_client();
    TenantApi::new(&client)
        .update_tenant(tenant_id, &req, token)
        .await
}

#[allow(dead_code)]
pub async fn delete(tenant_id: &str, token: &str) -> Result<MessageResponse> {
    let client = get_client();
    TenantApi::new(&client)
        .delete_tenant(tenant_id, token)
        .await
}

#[cfg(test)]
mod tests {
    use super::filter_active;
    use client_api::api::tenant::TenantInfo;

    fn tenant(id: &str, is_active: bool) -> TenantInfo {
        TenantInfo {
            id: id.to_string(),
            name: id.to_string(),
            slug: id.to_string(),
            description: None,
            user_count: 0,
            account_count: 0,
            status: if is_active { "active" } else { "inactive" }.to_string(),
            is_active,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn resource_selectors_exclude_inactive_tenants() {
        let result = filter_active(vec![tenant("active", true), tenant("inactive", false)]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "active");
    }
}
