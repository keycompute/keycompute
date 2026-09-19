#![allow(dead_code)]

use client_api::error::Result;
use client_api::{
    AdminApi,
    api::admin::{
        AccountInfo, AccountPage, AccountQueryParams, AccountRefreshResponse, AccountTestResponse,
        CreateAccountRequest, CreateModelBindingRequest, MessageResponse, ModelBindingInfo,
        ModelBindingPage, ModelBindingProbeRequest, ModelBindingProbeResponse,
        ModelBindingQueryParams, UpdateAccountRequest, UpdateModelBindingRequest,
    },
};

use super::api_client::get_client;

pub async fn list(params: Option<AccountQueryParams>, token: &str) -> Result<Vec<AccountInfo>> {
    let client = get_client();
    AdminApi::new(&client)
        .list_accounts(params.as_ref(), token)
        .await
}

pub async fn list_page(params: AccountQueryParams, token: &str) -> Result<AccountPage> {
    let client = get_client();
    AdminApi::new(&client)
        .list_accounts_page(Some(&params), token)
        .await
}

pub async fn create(req: CreateAccountRequest, token: &str) -> Result<AccountInfo> {
    let client = get_client();
    AdminApi::new(&client).create_account(&req, token).await
}

pub async fn update(id: &str, req: UpdateAccountRequest, token: &str) -> Result<AccountInfo> {
    let client = get_client();
    AdminApi::new(&client).update_account(id, &req, token).await
}

pub async fn delete(id: &str, token: &str) -> Result<MessageResponse> {
    let client = get_client();
    AdminApi::new(&client).delete_account(id, token).await
}

pub async fn test(id: &str, token: &str) -> Result<AccountTestResponse> {
    let client = get_client();
    AdminApi::new(&client).test_account(id, token).await
}

pub async fn refresh(id: &str, token: &str) -> Result<AccountRefreshResponse> {
    let client = get_client();
    AdminApi::new(&client).refresh_account(id, token).await
}

pub async fn list_model_bindings(
    params: Option<ModelBindingQueryParams>,
    token: &str,
) -> Result<ModelBindingPage> {
    let client = get_client();
    let api = AdminApi::new(&client);
    api.list_model_bindings_page(params.as_ref(), token).await
}

pub async fn create_model_binding(
    req: CreateModelBindingRequest,
    token: &str,
) -> Result<ModelBindingInfo> {
    let client = get_client();
    AdminApi::new(&client)
        .create_model_binding(&req, token)
        .await
}

pub async fn update_model_binding(
    id: &str,
    req: UpdateModelBindingRequest,
    token: &str,
) -> Result<ModelBindingInfo> {
    let client = get_client();
    AdminApi::new(&client)
        .update_model_binding(id, &req, token)
        .await
}

pub async fn delete_model_binding(
    id: &str,
    expected_revision: i64,
    token: &str,
) -> Result<MessageResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .delete_model_binding(id, expected_revision, token)
        .await
}

pub async fn delete_model_binding_with_revision(
    id: &str,
    expected_revision: i64,
    token: &str,
) -> Result<MessageResponse> {
    delete_model_binding(id, expected_revision, token).await
}

pub async fn probe_model_binding(
    id: &str,
    req: ModelBindingProbeRequest,
    token: &str,
) -> Result<ModelBindingProbeResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .probe_model_binding(id, &req, token)
        .await
}
