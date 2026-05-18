use client_api::{AdminApi, api::admin::MonitoringOverviewResponse, error::Result};

use super::api_client::get_client;

pub async fn overview(token: &str) -> Result<MonitoringOverviewResponse> {
    let client = get_client();
    AdminApi::new(&client).monitoring_overview(token).await
}
