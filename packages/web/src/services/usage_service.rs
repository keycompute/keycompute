use client_api::error::Result;
use client_api::{
    UsageApi,
    api::usage::{UsagePage, UsageQueryParams, UsageRecord, UsageStats},
};

use super::api_client::get_client;

#[allow(dead_code)]
pub async fn list(params: Option<UsageQueryParams>, token: &str) -> Result<Vec<UsageRecord>> {
    let client = get_client();
    UsageApi::new(&client)
        .get_my_usage(params.as_ref(), token)
        .await
}

pub async fn list_page(params: &UsageQueryParams, token: &str) -> Result<UsagePage> {
    let client = get_client();
    UsageApi::new(&client)
        .get_my_usage_page(params, token)
        .await
}

pub async fn stats(token: &str) -> Result<UsageStats> {
    let client = get_client();
    UsageApi::new(&client).get_usage_stats(token).await
}
