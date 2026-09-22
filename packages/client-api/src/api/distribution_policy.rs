//! Explicit tenant/root-targeted distribution policy API; no inferred tenant.
use crate::{
    client::ApiClient,
    error::{ClientError, Result},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BeneficiaryScope {
    Everyone,
    TenantMember,
}
#[derive(Debug, Clone, Deserialize)]
pub struct DistributionPolicy {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub beneficiary_scope: BeneficiaryScope,
    pub beneficiary_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub commission_rate: String,
    pub priority: i32,
    pub is_active: bool,
    pub effective_from: String,
    pub effective_until: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct PolicyPage {
    pub rules: Vec<DistributionPolicy>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}
#[derive(Debug, Clone, Serialize)]
pub struct CreatePolicy {
    pub name: String,
    pub description: Option<String>,
    pub commission_rate: String,
    pub beneficiary_scope: BeneficiaryScope,
    pub beneficiary_id: Option<Uuid>,
    pub priority: i32,
    pub effective_from: Option<String>,
    pub effective_until: Option<String>,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct PolicyPatch {
    pub expected_updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commission_rate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_until: Option<Option<String>>,
    pub reason: String,
}
impl PolicyPatch {
    pub fn new(expected_updated_at: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            expected_updated_at: expected_updated_at.into(),
            reason: reason.into(),
            name: None,
            description: None,
            commission_rate: None,
            priority: None,
            is_active: None,
            effective_until: None,
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct PolicyDeleted {
    pub id: Uuid,
    pub deleted: bool,
}
#[derive(Debug, Clone)]
pub struct DistributionPolicyApi {
    client: ApiClient,
    base: String,
}
impl DistributionPolicyApi {
    pub fn tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        Self::new(client, tenant, false)
    }
    pub fn platform_tenant(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        Self::new(client, tenant, true)
    }
    fn new(client: &ApiClient, tenant: Uuid, platform: bool) -> Result<Self> {
        if tenant.is_nil() {
            return Err(ClientError::Config("explicit tenant required".into()));
        }
        let base = if platform {
            format!("/api/v1/platform/distribution/tenants/{tenant}/rules")
        } else {
            format!("/api/v1/tenants/{tenant}/distribution/rules")
        };
        Ok(Self {
            client: client.clone(),
            base,
        })
    }
    fn resource(&self, id: Uuid) -> Result<String> {
        if id.is_nil() {
            return Err(ClientError::Config("real policy ID required".into()));
        }
        Ok(format!("{}/{id}", self.base))
    }
    pub async fn list(&self, page: u32, size: u32, token: &str) -> Result<PolicyPage> {
        if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&size) {
            return Err(ClientError::Config("invalid policy pagination".into()));
        }
        self.client
            .get_json(
                &format!("{}?page={page}&page_size={size}", self.base),
                Some(token),
            )
            .await
    }
    pub async fn get(&self, id: Uuid, token: &str) -> Result<DistributionPolicy> {
        self.client.get_json(&self.resource(id)?, Some(token)).await
    }
    pub async fn create(&self, body: &CreatePolicy, token: &str) -> Result<DistributionPolicy> {
        self.client.post_json(&self.base, body, Some(token)).await
    }
    pub async fn patch(
        &self,
        id: Uuid,
        body: &PolicyPatch,
        token: &str,
    ) -> Result<DistributionPolicy> {
        let r = self
            .client
            .request_with_auth(reqwest::Method::PATCH, &self.resource(id)?, Some(token))
            .await?;
        self.client.send_and_parse(r.json(body)).await
    }
    pub async fn delete(
        &self,
        id: Uuid,
        revision: &str,
        reason: &str,
        token: &str,
    ) -> Result<PolicyDeleted> {
        let r = self
            .client
            .request_with_auth(reqwest::Method::DELETE, &self.resource(id)?, Some(token))
            .await?;
        self.client
            .send_and_parse(
                r.json(&serde_json::json!({"expected_updated_at":revision,"reason":reason})),
            )
            .await
    }
    pub async fn set_default(
        &self,
        name: &str,
        rate: &str,
        reason: &str,
        token: &str,
    ) -> Result<DistributionPolicy> {
        self.client
            .post_json(
                &format!("{}/default", self.base),
                &serde_json::json!({"name":name,"commission_rate":rate,"reason":reason}),
                Some(token),
            )
            .await
    }
}
