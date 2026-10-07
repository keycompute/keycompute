//! Explicitly scoped distribution record reporting.
//!
//! Platform reads always carry the target tenant in the route. This avoids the
//! legacy console behaviour where a missing selected tenant was interpreted as
//! an incomplete request.
use crate::{ApiClient, ClientError, Result};
use serde::Deserialize;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DistributionRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub beneficiary_scope: String,
    pub beneficiary_id: Option<Uuid>,
    pub usage_log_id: Uuid,
    pub referred_id: Uuid,
    pub amount: String,
    pub currency: String,
    pub commission: String,
    pub share_ratio: String,
    pub level: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DistributionRecordPage {
    pub records: Vec<DistributionRecord>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

#[derive(Debug, Clone)]
pub struct DistributionReportingApi {
    client: ApiClient,
    tenant: Uuid,
    base: String,
}

fn invalid() -> ClientError {
    ClientError::InvalidResponse(
        "Distribution report did not match the requested tenant or pagination".into(),
    )
}

impl DistributionReportingApi {
    pub fn platform_tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        if tenant.is_nil() {
            return Err(ClientError::Config("explicit tenant required".into()));
        }
        Ok(Self {
            client: client.clone(),
            tenant,
            base: format!("/api/v1/platform/distribution/tenants/{tenant}"),
        })
    }

    pub async fn records(
        &self,
        page: u32,
        page_size: u32,
        token: &str,
    ) -> Result<DistributionRecordPage> {
        if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&page_size) {
            return Err(ClientError::Config(
                "invalid distribution report pagination".into(),
            ));
        }
        let result: DistributionRecordPage = self
            .client
            .get_json_fresh(
                &format!("{}/records?page={page}&page_size={page_size}", self.base),
                Some(token),
            )
            .await?;
        let expected_pages = if result.total <= 0 {
            0
        } else {
            let page_size = i64::from(page_size);
            result.total / page_size + i64::from(result.total % page_size != 0)
        };
        if result.page != i64::from(page)
            || result.page_size != i64::from(page_size)
            || result.total < 0
            || result.total_pages != expected_pages
            || result.records.len() > page_size as usize
            || result.records.len() as i64 > result.total
        {
            return Err(invalid());
        }
        let mut ids = HashSet::new();
        for row in &result.records {
            if row.tenant_id != self.tenant
                || row.id.is_nil()
                || row.usage_log_id.is_nil()
                || row.referred_id.is_nil()
                || !ids.insert(row.id)
            {
                return Err(invalid());
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_reporting_requires_an_explicit_tenant() {
        let client = ApiClient::new(crate::ClientConfig::new("http://localhost:8080")).unwrap();
        assert!(DistributionReportingApi::platform_tenant(&client, Uuid::nil()).is_err());
    }
}
