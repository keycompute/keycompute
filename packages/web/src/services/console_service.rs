use super::api_client::get_client;
use client_api::Result;
use client_api::api::console::{ConsoleApi, DashboardSummary, TrendQuery, UsageTrend};
pub async fn dashboard(token: &str) -> Result<DashboardSummary> {
    ConsoleApi::new(&get_client()).dashboard(token).await
}
pub async fn trend(token: &str) -> Result<UsageTrend> {
    ConsoleApi::new(&get_client())
        .usage_trend(&TrendQuery::default(), token)
        .await
}
