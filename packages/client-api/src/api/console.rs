//! Compact console read models. Monetary values remain decimal strings.
use super::common::encode_query_value;
use crate::{ApiClient, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct DashboardSummary {
    pub as_of: String,
    pub stats: DashboardStats,
    pub active_key_count: u64,
    pub active_keys: Vec<KeyPreview>,
    pub recent_usage: Vec<RecentUsage>,
    pub recent_orders: Vec<RecentOrder>,
    pub trend: UsageTrend,
}
#[derive(Debug, Clone, Deserialize)]
pub struct DashboardStats {
    pub total_requests: i64,
    pub total_tokens: i64,
    pub total_cost: String,
    pub period: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct KeyPreview {
    pub id: String,
    pub name: String,
    pub key_preview: String,
    pub last_used_at: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RecentUsage {
    pub id: String,
    pub request_id: String,
    pub model: String,
    pub cost: String,
    pub status: String,
    pub created_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RecentOrder {
    pub id: String,
    pub amount: String,
    pub currency: String,
    pub status: String,
    pub created_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct UsageTrend {
    pub from: String,
    pub to: String,
    pub granularity: String,
    pub as_of: String,
    pub buckets: Vec<TrendBucket>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TrendBucket {
    pub start: String,
    pub requests: i64,
    pub total_tokens: i64,
    pub total_cost: String,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct TrendQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub granularity: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ConsoleApi {
    client: ApiClient,
}
impl ConsoleApi {
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }
    pub async fn dashboard(&self, token: &str) -> Result<DashboardSummary> {
        self.client
            .get_json("/api/v1/dashboard/overview", Some(token))
            .await
    }
    pub async fn usage_trend(&self, query: &TrendQuery, token: &str) -> Result<UsageTrend> {
        let mut parts = Vec::new();
        for (name, value) in [
            ("from", &query.from),
            ("to", &query.to),
            ("granularity", &query.granularity),
        ] {
            if let Some(value) = value {
                parts.push(format!("{name}={}", encode_query_value(value)));
            }
        }
        let path = if parts.is_empty() {
            "/api/v1/usage/trend".to_string()
        } else {
            format!("/api/v1/usage/trend?{}", parts.join("&"))
        };
        self.client.get_json(&path, Some(token)).await
    }
}
