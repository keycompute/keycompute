//! Explicit platform metadata/aggregate API; no raw-resource or write operations.
use super::common::encode_query_value;
use crate::{
    ApiClient,
    error::{ClientError, Result},
};
use serde::Deserialize;
use uuid::Uuid;
#[derive(Debug, Clone, Deserialize)]
pub struct TenantHealth {
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: String,
    pub status: String,
    pub default_rpm_limit: i32,
    pub default_tpm_limit: i32,
    pub active_members: i64,
    pub active_admins: i64,
    pub suspended_members: i64,
    pub provider_accounts: i64,
    pub enabled_accounts: i64,
    pub online_nodes: i64,
    pub excluded_nodes: i64,
    pub queued_tasks: i64,
    pub leased_tasks: i64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct TenantHealthPage {
    pub items: Vec<TenantHealth>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub as_of: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct CurrencyOperations {
    pub currency: String,
    pub requests: i64,
    pub successful_requests: i64,
    pub total_tokens: String,
    pub billed_amount: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct UsageOperations {
    pub from: String,
    pub to: String,
    pub currencies: Vec<CurrencyOperations>,
    pub as_of: String,
}
#[derive(Debug, Clone, Default)]
pub struct TenantHealthQuery {
    pub search: Option<String>,
    pub status: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}
#[derive(Debug, Clone, Default)]
pub struct UsageOperationsQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}
#[derive(Debug, Clone)]
pub struct PlatformOperationsApi {
    client: ApiClient,
}
fn tenant_path(tenant: Uuid) -> Result<String> {
    if tenant.is_nil() {
        return Err(ClientError::Config(
            "An explicit tenant target is required".into(),
        ));
    }
    Ok(format!("/api/v1/platform/operations/tenants/{tenant}"))
}
fn with_query(path: &str, values: Vec<(&str, String)>) -> String {
    if values.is_empty() {
        path.to_owned()
    } else {
        format!(
            "{path}?{}",
            values
                .into_iter()
                .map(|(k, v)| format!("{k}={}", encode_query_value(&v)))
                .collect::<Vec<_>>()
                .join("&")
        )
    }
}
impl PlatformOperationsApi {
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }
    pub async fn tenants(
        &self,
        query: &TenantHealthQuery,
        token: &str,
    ) -> Result<TenantHealthPage> {
        let mut values = Vec::new();
        if let Some(v) = &query.search {
            values.push(("search", v.clone()));
        }
        if let Some(v) = &query.status {
            values.push(("status", v.clone()));
        }
        if let Some(v) = query.limit {
            values.push(("limit", v.to_string()));
        }
        if let Some(v) = query.offset {
            values.push(("offset", v.to_string()));
        }
        self.client
            .get_json_fresh(
                &with_query("/api/v1/platform/operations/tenants", values),
                Some(token),
            )
            .await
    }
    pub async fn tenant(&self, tenant: Uuid, token: &str) -> Result<TenantHealth> {
        self.client
            .get_json_fresh(&tenant_path(tenant)?, Some(token))
            .await
    }
    pub async fn platform_usage(
        &self,
        query: &UsageOperationsQuery,
        token: &str,
    ) -> Result<UsageOperations> {
        self.usage("/api/v1/platform/operations/usage", query, token)
            .await
    }
    pub async fn tenant_usage(
        &self,
        tenant: Uuid,
        query: &UsageOperationsQuery,
        token: &str,
    ) -> Result<UsageOperations> {
        self.usage(&format!("{}/usage", tenant_path(tenant)?), query, token)
            .await
    }
    async fn usage(
        &self,
        path: &str,
        query: &UsageOperationsQuery,
        token: &str,
    ) -> Result<UsageOperations> {
        let values = [("from", &query.from), ("to", &query.to)]
            .into_iter()
            .filter_map(|(k, v)| v.clone().map(|v| (k, v)))
            .collect();
        self.client
            .get_json_fresh(&with_query(path, values), Some(token))
            .await
    }
    pub async fn capacity(&self, token: &str) -> Result<serde_json::Value> {
        self.client
            .get_json_fresh("/api/v1/platform/operations/capacity", Some(token))
            .await
    }
}
