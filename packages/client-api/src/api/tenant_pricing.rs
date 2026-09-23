//! Explicit current-tenant pricing, distinct from platform pricing administration.
//! Monetary values remain strings. Reads are fresh and commands never auto-replay.
use crate::{ApiClient, ClientError, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingDimension {
    #[serde(rename = "provideraccount")]
    ProviderAccount,
    #[serde(rename = "node")]
    Node,
}
impl BillingDimension {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderAccount => "provideraccount",
            Self::Node => "node",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingScope {
    Tenant,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TenantPrice {
    pub id: Uuid,
    pub scope_type: PricingScope,
    pub tenant_id: Uuid,
    pub model_name: String,
    pub billing_dimension: BillingDimension,
    pub currency: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub is_default: bool,
    pub is_effective: bool,
    pub effective_from: String,
    pub effective_until: Option<String>,
    pub created_at: String,
    pub version: i64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct PricingPage {
    pub pricing: Vec<TenantPrice>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
    pub total_pages: i64,
}
#[derive(Debug, Clone, Serialize)]
pub struct CreateTenantPrice {
    pub model_name: String,
    pub billing_dimension: BillingDimension,
    pub currency: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub is_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_until: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct UpdateTenantPrice {
    pub expected_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_price_per_1k: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_price_per_1k: Option<String>,
    /// Absent leaves the existing end date unchanged, never implicitly clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_until: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct DeletedPrice {
    pub success: bool,
    pub pricing_id: Uuid,
}

fn invalid() -> ClientError {
    ClientError::InvalidResponse(
        "Pricing response did not match the requested tenant/resource".into(),
    )
}
fn real(id: Uuid) -> Result<()> {
    if id.is_nil() {
        Err(ClientError::Config(
            "An explicit nonzero resource ID is required".into(),
        ))
    } else {
        Ok(())
    }
}
/// Console input accepts bounded plain decimals only; no float rounding or exponent expansion.
/// The server remains authoritative for its broader decimal syntax and pricing rules.
pub fn validate_decimal(value: &str) -> Result<()> {
    let value = value.trim();
    let (whole, frac) = value.split_once('.').unwrap_or((value, ""));
    if value.is_empty()
        || value.len() > 64
        || whole.is_empty()
        || !whole.bytes().all(|v| v.is_ascii_digit())
        || !frac.bytes().all(|v| v.is_ascii_digit())
        || whole.trim_start_matches('0').len() > 10
        || frac.trim_end_matches('0').len() > 10
    {
        return Err(ClientError::Config(
            "Use a nonnegative plain decimal fitting DECIMAL(20,10)".into(),
        ));
    }
    Ok(())
}
fn timestamp(value: &Option<String>) -> Result<()> {
    if value
        .as_ref()
        .is_some_and(|v| v.is_empty() || v.len() > 128 || v.chars().any(char::is_control))
    {
        return Err(ClientError::Config(
            "Use a bounded RFC3339 pricing time".into(),
        ));
    }
    // Calendar semantics are checked by the server; the Web editor also parses RFC3339.
    Ok(())
}
#[derive(Debug, Clone)]
pub struct TenantPricingApi {
    client: ApiClient,
    tenant: Uuid,
    base: String,
}
impl TenantPricingApi {
    pub fn new(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        real(tenant)?;
        Ok(Self {
            client: client.clone(),
            tenant,
            base: format!("/api/v1/tenants/{tenant}/pricing"),
        })
    }
    fn check(&self, row: TenantPrice, id: Option<Uuid>) -> Result<TenantPrice> {
        if row.tenant_id != self.tenant
            || row.id.is_nil()
            || row.version <= 0
            || id.is_some_and(|id| row.id != id)
        {
            return Err(invalid());
        }
        Ok(row)
    }
    pub async fn list(
        &self,
        page: u32,
        size: u32,
        search: &str,
        token: &str,
    ) -> Result<PricingPage> {
        if !(1..=1_000_000).contains(&page) || !(1..=100).contains(&size) || search.len() > 255 {
            return Err(ClientError::Config(
                "Invalid pricing pagination or search".into(),
            ));
        }
        let path = format!(
            "{}?page={page}&page_size={size}&search={}",
            self.base,
            super::common::encode_query_value(search)
        );
        let result: PricingPage = self.client.get_json_fresh(&path, Some(token)).await?;
        if result.page != page
            || result.page_size != size
            || result.total < 0
            || result.total_pages < 0
            || result.pricing.len() > size as usize
        {
            return Err(invalid());
        }
        for row in &result.pricing {
            self.check(row.clone(), None)?;
        }
        Ok(result)
    }
    pub async fn detail(&self, id: Uuid, token: &str) -> Result<TenantPrice> {
        real(id)?;
        self.check(
            self.client
                .get_json_fresh(&format!("{}/{id}", self.base), Some(token))
                .await?,
            Some(id),
        )
    }
    pub async fn create(&self, body: &CreateTenantPrice, token: &str) -> Result<TenantPrice> {
        if body.model_name.trim().is_empty()
            || body.model_name.len() > 255
            || body.model_name.chars().any(char::is_control)
            || body.currency.len() != 3
            || !body.currency.bytes().all(|b| b.is_ascii_uppercase())
        {
            return Err(ClientError::Config(
                "A model name and three-letter uppercase currency are required".into(),
            ));
        }
        validate_decimal(&body.input_price_per_1k)?;
        validate_decimal(&body.output_price_per_1k)?;
        timestamp(&body.effective_from)?;
        timestamp(&body.effective_until)?;
        self.check(
            self.client.post_json(&self.base, body, Some(token)).await?,
            None,
        )
    }
    pub async fn update(
        &self,
        id: Uuid,
        body: &UpdateTenantPrice,
        token: &str,
    ) -> Result<TenantPrice> {
        real(id)?;
        if body.expected_version <= 0 {
            return Err(ClientError::Config(
                "A displayed positive pricing version is required".into(),
            ));
        }
        for value in [&body.input_price_per_1k, &body.output_price_per_1k]
            .into_iter()
            .flatten()
        {
            validate_decimal(value)?;
        }
        timestamp(&body.effective_until)?;
        let request = self
            .client
            .request_with_auth(Method::PATCH, &format!("{}/{id}", self.base), Some(token))
            .await?;
        self.check(
            self.client.send_and_parse(request.json(body)).await?,
            Some(id),
        )
    }
    pub async fn delete(&self, id: Uuid, token: &str) -> Result<DeletedPrice> {
        real(id)?;
        let result: DeletedPrice = self
            .client
            .delete_json(&format!("{}/{id}", self.base), Some(token))
            .await?;
        if !result.success || result.pricing_id != id {
            return Err(invalid());
        }
        Ok(result)
    }
    pub async fn make_default(&self, id: Uuid, token: &str) -> Result<TenantPrice> {
        real(id)?;
        self.check(
            self.client
                .post_json(
                    &format!("{}/{id}/make-default", self.base),
                    &serde_json::json!({}),
                    Some(token),
                )
                .await?,
            Some(id),
        )
    }
}
