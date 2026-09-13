#![allow(dead_code)]

use client_api::error::Result;
use client_api::{
    DistributionApi,
    api::distribution::{
        DistributionEarnings, DistributionQueryParams, DistributionRecordPage, InviteLinkResponse,
        ReferralCodeResponse, ReferralInfo,
    },
};

use super::api_client::get_client;

pub async fn get_earnings(token: &str) -> Result<DistributionEarnings> {
    let client = get_client();
    DistributionApi::new(&client)
        .get_my_distribution_earnings(token)
        .await
}

pub async fn get_referrals(token: &str) -> Result<Vec<ReferralInfo>> {
    let client = get_client();
    DistributionApi::new(&client).get_my_referrals(token).await
}

pub async fn list_records_page(
    params: &DistributionQueryParams,
    token: &str,
) -> Result<DistributionRecordPage> {
    let client = get_client();
    DistributionApi::new(&client)
        .list_distribution_records_page(params, token)
        .await
}

pub async fn get_referral_code(token: &str) -> Result<ReferralCodeResponse> {
    let client = get_client();
    DistributionApi::new(&client)
        .get_my_referral_code(token)
        .await
}

pub async fn generate_invite_link(token: &str) -> Result<InviteLinkResponse> {
    let client = get_client();
    DistributionApi::new(&client)
        .generate_invite_link(token)
        .await
}
