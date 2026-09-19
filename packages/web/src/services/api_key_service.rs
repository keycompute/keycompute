use client_api::error::Result;
use client_api::{
    ApiKeyApi,
    api::api_key::{
        ApiKeyPage, ApiKeyQueryParams, CreateApiKeyRequest, CreateApiKeyResponse, MessageResponse,
    },
};

use super::api_client::get_client;

pub async fn list_page(
    include_revoked: bool,
    page: u32,
    page_size: u32,
    token: &str,
) -> Result<ApiKeyPage> {
    let client = get_client();
    ApiKeyApi::new(&client)
        .list_my_api_keys_page(
            &ApiKeyQueryParams::new()
                .with_include_revoked(include_revoked)
                .with_page(page as i32)
                .with_page_size(page_size as i32),
            token,
        )
        .await
}

pub async fn create(name: &str, token: &str) -> Result<CreateApiKeyResponse> {
    let client = get_client();
    ApiKeyApi::new(&client)
        .create_api_key(&CreateApiKeyRequest::new(name), token)
        .await
}

pub async fn delete(id: &str, token: &str) -> Result<MessageResponse> {
    let client = get_client();
    ApiKeyApi::new(&client).delete_api_key(id, token).await
}
