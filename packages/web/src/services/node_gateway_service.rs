use client_api::{AdminApi, api::admin::NodeGatewayOverviewResponse, error::Result};

use super::api_client::get_client;

pub async fn overview(token: &str) -> Result<NodeGatewayOverviewResponse> {
    let client = get_client();
    AdminApi::new(&client).node_gateway_overview(token).await
}
