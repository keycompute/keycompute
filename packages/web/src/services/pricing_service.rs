#![allow(dead_code)]

use client_api::error::Result;
use client_api::{
    AdminApi,
    api::admin::{
        CreatePricingRequest, CreatePricingResponse, MessageResponse, PricingInfo,
        SetDefaultPricingRequest, UpdatePricingRequest, UpdatePricingResponse,
    },
};

use super::api_client::get_client;

pub async fn list(token: &str) -> Result<Vec<PricingInfo>> {
    let client = get_client();
    AdminApi::new(&client).list_pricing(token).await
}

pub async fn create(req: CreatePricingRequest, token: &str) -> Result<CreatePricingResponse> {
    let client = get_client();
    AdminApi::new(&client).create_pricing(&req, token).await
}

pub async fn update(
    id: &str,
    req: UpdatePricingRequest,
    token: &str,
) -> Result<UpdatePricingResponse> {
    let client = get_client();
    AdminApi::new(&client).update_pricing(id, &req, token).await
}

pub async fn delete(id: &str, token: &str) -> Result<MessageResponse> {
    let client = get_client();
    AdminApi::new(&client).delete_pricing(id, token).await
}

pub async fn set_defaults(req: SetDefaultPricingRequest, token: &str) -> Result<MessageResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .set_default_pricing(&req, token)
        .await
}
