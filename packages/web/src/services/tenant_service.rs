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
