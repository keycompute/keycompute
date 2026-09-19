#![allow(dead_code)]

use client_api::error::Result;
use client_api::{
    AdminApi,
    api::admin::{
        AccountInfo, AccountPage, AccountQueryParams, AccountRefreshResponse, AccountTestResponse,
        CreateAccountRequest, CreatePassthroughBindingRequest, MessageResponse,
        PassthroughAccountOptions, PassthroughAccountOptionsQuery, PassthroughBindingInfo,
        PassthroughBindingPage, PassthroughBindingProbeRequest, PassthroughBindingProbeResponse,
        PassthroughBindingQueryParams, UpdateAccountRequest, UpdatePassthroughBindingRequest,
    },
};

use super::api_client::get_client;

pub async fn list(params: Option<AccountQueryParams>, token: &str) -> Result<Vec<AccountInfo>> {
    let client = get_client();
    AdminApi::new(&client)
        .list_accounts(params.as_ref(), token)
        .await
}

pub async fn list_passthrough_bindings(
    params: Option<PassthroughBindingQueryParams>,
    token: &str,
) -> Result<PassthroughBindingPage> {
    let client = get_client();
    AdminApi::new(&client)
        .list_passthrough_bindings_page(params.as_ref(), token)
        .await
}

pub async fn passthrough_binding_options(
    params: PassthroughAccountOptionsQuery,
    token: &str,
) -> Result<PassthroughAccountOptions> {
    let client = get_client();
    AdminApi::new(&client)
        .passthrough_binding_options(&params, token)
        .await
}

pub async fn create_passthrough_binding(
    req: CreatePassthroughBindingRequest,
    token: &str,
) -> Result<PassthroughBindingInfo> {
    let client = get_client();
    AdminApi::new(&client)
        .create_passthrough_binding(&req, token)
        .await
}

pub async fn update_passthrough_binding(
    id: &str,
    req: UpdatePassthroughBindingRequest,
    token: &str,
) -> Result<PassthroughBindingInfo> {
    let client = get_client();
    AdminApi::new(&client)
        .update_passthrough_binding(id, &req, token)
        .await
}

pub async fn delete_passthrough_binding(
    id: &str,
    revision: i64,
    token: &str,
) -> Result<MessageResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .delete_passthrough_binding(id, revision, token)
        .await
}

pub async fn probe_passthrough_binding(
    id: &str,
    req: PassthroughBindingProbeRequest,
    token: &str,
) -> Result<PassthroughBindingProbeResponse> {
    let client = get_client();
    AdminApi::new(&client)
        .probe_passthrough_binding(id, &req, token)
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
