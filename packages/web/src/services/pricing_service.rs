#![allow(dead_code)]

use client_api::error::Result;
use client_api::{
    AdminApi,
    api::admin::{
        BatchDefaultPricingResponse, CreatePricingRequest, CreatePricingResponse,
        DeletePricingResponse, MakeDefaultPricingResponse, PricingInfo, PricingPage,
        PricingQueryParams, PricingTarget, SetDefaultPricingRequest, UpdatePricingRequest,
        UpdatePricingResponse,
    },
};

use super::api_client::get_client;

/// Ownership is declared explicitly by the server, not inferred from an empty UUID.
pub fn is_platform_price(row: &PricingInfo) -> bool {
    row.target()
        .is_ok_and(|target| target == PricingTarget::Platform)
}

pub async fn list(target: PricingTarget, token: &str) -> Result<Vec<PricingInfo>> {
    AdminApi::new(&get_client())
        .list_pricing(target, token)
        .await
}

pub async fn list_page(params: &PricingQueryParams, token: &str) -> Result<PricingPage> {
    let client = get_client();
    AdminApi::new(&client)
        .list_pricing_page(params, token)
        .await
}

pub async fn create(req: CreatePricingRequest, token: &str) -> Result<CreatePricingResponse> {
    let client = get_client();
    AdminApi::new(&client).create_pricing(&req, token).await
}

pub async fn update(
    target: PricingTarget,
    id: &str,
    req: UpdatePricingRequest,
    token: &str,
) -> Result<UpdatePricingResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .update_pricing(target, id, &req, token)
        .await
}

pub async fn delete(target: PricingTarget, id: &str, token: &str) -> Result<DeletePricingResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .delete_pricing(target, id, token)
        .await
}

pub async fn make_default(
    target: PricingTarget,
    id: &str,
    token: &str,
) -> Result<MakeDefaultPricingResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .make_pricing_default(target, id, token)
        .await
}

pub async fn set_defaults(
    target: PricingTarget,
    req: SetDefaultPricingRequest,
    token: &str,
) -> Result<BatchDefaultPricingResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .set_default_pricing(target, &req, token)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_platform_scope_is_global() {
        let mut row: PricingInfo=serde_json::from_value(serde_json::json!({
            "id":"11111111-1111-4111-8111-111111111111","scope_type":"platform","tenant_id":null,
            "model_name":"fixture","billing_dimension":"node","input_price_per_1k":"0.1",
            "output_price_per_1k":"0.2","currency":"CNY","is_default":true,"is_effective":true,
            "effective_from":"2026-01-01T00:00:00Z","effective_until":null,"created_at":"2026-01-01T00:00:00Z","version":3
        })).unwrap();
        assert!(is_platform_price(&row));
        row.tenant_id = Some(uuid::Uuid::nil().to_string());
        assert!(!is_platform_price(&row));
        row.scope_type = client_api::api::admin::PricingScopeType::Tenant;
        assert!(!is_platform_price(&row));
        row.tenant_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(!is_platform_price(&row));
    }
}
