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
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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
    tenant: Uuid,
    base: String,
}
fn invalid() -> ClientError {
    ClientError::InvalidResponse(
        "Distribution policy response did not match the requested tenant/resource".into(),
    )
}
fn bounded_text(value: &str, max: usize, label: &str) -> Result<()> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > max || v.chars().any(char::is_control) {
        Err(ClientError::Config(format!(
            "A bounded {label} is required"
        )))
    } else {
        Ok(())
    }
}
pub fn validate_commission_rate(value: &str) -> Result<()> {
    let v = value.trim();
    if v.is_empty() || v.len() > 16 || v.starts_with(['+', '-']) {
        return Err(ClientError::Config(
            "Use a plain commission rate between 0 and 1 with at most four decimal places".into(),
        ));
    }
    let mut parts = v.split('.');
    let whole = parts.next().unwrap_or_default();
    let frac = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || !matches!(whole, "0" | "1")
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || frac.len() > 4
        || (whole == "1" && frac.bytes().any(|b| b != b'0'))
    {
        return Err(ClientError::Config(
            "Use a plain commission rate between 0 and 1 with at most four decimal places".into(),
        ));
    }
    Ok(())
}
fn validate_reason(value: &str) -> Result<()> {
    bounded_text(value, 500, "reason")
}
fn description(value: &Option<String>) -> Result<()> {
    if value.as_ref().is_some_and(|v| {
        v.chars().count() > 4096 || v.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
    }) {
        Err(ClientError::Config(
            "Policy description is too long or invalid".into(),
        ))
    } else {
        Ok(())
    }
}
fn timestamp(value: &Option<String>) -> Result<()> {
    if value
        .as_ref()
        .is_some_and(|v| v.is_empty() || v.len() > 128 || v.chars().any(char::is_control))
    {
        Err(ClientError::Config(
            "Use a bounded RFC3339 policy time".into(),
        ))
    } else {
        Ok(())
    }
}
fn create_input(body: &CreatePolicy) -> Result<()> {
    bounded_text(&body.name, 255, "policy name")?;
    description(&body.description)?;
    validate_reason(&body.reason)?;
    validate_commission_rate(&body.commission_rate)?;
    if !(-1000..=1000).contains(&body.priority) {
        return Err(ClientError::Config(
            "Policy priority must be within -1000..1000".into(),
        ));
    }
    match (body.beneficiary_scope, body.beneficiary_id) {
        (BeneficiaryScope::Everyone, None) => {}
        (BeneficiaryScope::TenantMember, Some(id)) if !id.is_nil() => {}
        _ => {
            return Err(ClientError::Config(
                "Beneficiary scope and member ID do not agree".into(),
            ));
        }
    }
    timestamp(&body.effective_from)?;
    timestamp(&body.effective_until)?;
    Ok(())
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
            tenant,
            base,
        })
    }
    fn resource(&self, id: Uuid) -> Result<String> {
        if id.is_nil() {
            return Err(ClientError::Config("real policy ID required".into()));
        }
        Ok(format!("{}/{id}", self.base))
    }
    fn check(&self, row: DistributionPolicy, id: Option<Uuid>) -> Result<DistributionPolicy> {
        if row.tenant_id != self.tenant
            || row.id.is_nil()
            || id.is_some_and(|id| id != row.id)
            || row.updated_at.is_empty()
            || row.created_at.is_empty()
        {
            return Err(invalid());
        }
        validate_commission_rate(&row.commission_rate).map_err(|_| invalid())?;
        bounded_text(&row.name, 255, "policy name").map_err(|_| invalid())?;
        description(&row.description).map_err(|_| invalid())?;
        if !(-1000..=1000).contains(&row.priority) {
            return Err(invalid());
        }
        timestamp(&Some(row.effective_from.clone())).map_err(|_| invalid())?;
        timestamp(&row.effective_until).map_err(|_| invalid())?;
        match (row.beneficiary_scope, row.beneficiary_id) {
            (BeneficiaryScope::Everyone, None) => {}
            (BeneficiaryScope::TenantMember, Some(id)) if !id.is_nil() => {}
            _ => return Err(invalid()),
        }
        Ok(row)
    }
    fn check_page(&self, result: &PolicyPage, page: u32, size: u32) -> Result<()> {
        let expected = (result.total + i64::from(size) - 1) / i64::from(size);
        if result.page != i64::from(page)
            || result.page_size != i64::from(size)
            || result.total < 0
            || result.total_pages != expected
            || result.rules.len() > size as usize
            || result.rules.len() as i64 > result.total
        {
            return Err(invalid());
        }
        let mut ids = std::collections::HashSet::new();
        for row in &result.rules {
            self.check(row.clone(), None)?;
            if !ids.insert(row.id) {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub async fn list(&self, page: u32, size: u32, token: &str) -> Result<PolicyPage> {
        if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&size) {
            return Err(ClientError::Config("invalid policy pagination".into()));
        }
        let result: PolicyPage = self
            .client
            .get_json_fresh(
                &format!("{}?page={page}&page_size={size}", self.base),
                Some(token),
            )
            .await?;
        self.check_page(&result, page, size)?;
        Ok(result)
    }
    pub async fn get(&self, id: Uuid, token: &str) -> Result<DistributionPolicy> {
        let row = self
            .client
            .get_json_fresh(&self.resource(id)?, Some(token))
            .await?;
        self.check(row, Some(id))
    }
    pub async fn create(&self, body: &CreatePolicy, token: &str) -> Result<DistributionPolicy> {
        create_input(body)?;
        let row = self.client.post_json(&self.base, body, Some(token)).await?;
        self.check(row, None)
    }
    pub async fn patch(
        &self,
        id: Uuid,
        body: &PolicyPatch,
        token: &str,
    ) -> Result<DistributionPolicy> {
        if body.expected_updated_at.trim().is_empty() {
            return Err(ClientError::Config(
                "The displayed policy revision is required".into(),
            ));
        }
        validate_reason(&body.reason)?;
        if let Some(v) = &body.name {
            bounded_text(v, 255, "policy name")?;
        }
        if let Some(v) = &body.description {
            description(v)?;
        }
        if let Some(v) = &body.commission_rate {
            validate_commission_rate(v)?;
        }
        if body.priority.is_some_and(|v| !(-1000..=1000).contains(&v)) {
            return Err(ClientError::Config(
                "Policy priority must be within -1000..1000".into(),
            ));
        }
        if let Some(v) = &body.effective_until {
            timestamp(v)?;
        }
        let r = self
            .client
            .request_with_auth(reqwest::Method::PATCH, &self.resource(id)?, Some(token))
            .await?;
        let row = self.client.send_and_parse(r.json(body)).await?;
        self.check(row, Some(id))
    }
    pub async fn delete(
        &self,
        id: Uuid,
        revision: &str,
        reason: &str,
        token: &str,
    ) -> Result<PolicyDeleted> {
        if revision.trim().is_empty() {
            return Err(ClientError::Config(
                "The displayed policy revision is required".into(),
            ));
        }
        validate_reason(reason)?;
        let r = self
            .client
            .request_with_auth(reqwest::Method::DELETE, &self.resource(id)?, Some(token))
            .await?;
        let result: PolicyDeleted = self
            .client
            .send_and_parse(
                r.json(&serde_json::json!({"expected_updated_at":revision,"reason":reason})),
            )
            .await?;
        if !result.deleted || result.id != id {
            return Err(invalid());
        }
        Ok(result)
    }
    pub async fn set_default(
        &self,
        name: &str,
        rate: &str,
        reason: &str,
        token: &str,
    ) -> Result<DistributionPolicy> {
        bounded_text(name, 255, "policy name")?;
        validate_commission_rate(rate)?;
        validate_reason(reason)?;
        let row = self
            .client
            .post_json(
                &format!("{}/default", self.base),
                &serde_json::json!({"name":name,"commission_rate":rate,"reason":reason}),
                Some(token),
            )
            .await?;
        self.check(row, None)
    }
}
