//! Read-only tenant financial reporting. No platform fallback or financial writes.
use crate::{ApiClient, ClientError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportQuery {
    pub page: u32,
    pub page_size: u32,
    pub owner_user_id: Option<Uuid>,
}
impl Default for ReportQuery {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: 20,
            owner_user_id: None,
        }
    }
}
impl ReportQuery {
    fn query(&self) -> Result<String> {
        if !(1..=1_000_000).contains(&self.page)
            || !(1..=100).contains(&self.page_size)
            || self.owner_user_id.is_some_and(|v| v.is_nil())
        {
            return Err(ClientError::Config(
                "Use bounded pagination and a real optional owner".into(),
            ));
        }
        Ok(format!(
            "page={}&page_size={}{}",
            self.page,
            self.page_size,
            self.owner_user_id
                .map(|v| format!("&owner_user_id={v}"))
                .unwrap_or_default()
        ))
    }
}
/// Calendar semantics are validated by the server. Values are encoded as single query parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportWindow {
    pub from: String,
    pub to: String,
}
impl ReportWindow {
    fn query(&self) -> Result<String> {
        for value in [&self.from, &self.to] {
            if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
                return Err(ClientError::Config(
                    "A bounded report timestamp is required".into(),
                ));
            }
        }
        Ok(format!(
            "from={}&to={}",
            super::common::encode_query_value(&self.from),
            super::common::encode_query_value(&self.to)
        ))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentState {
    Pending,
    Paid,
    Failed,
    Closed,
}
impl PaymentState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Paid => "paid",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReportPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
    pub total_pages: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantUsageRecord {
    pub id: Uuid,
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub produce_ai_key_id: Uuid,
    pub model_name: String,
    pub provider_name: String,
    pub account_id: Uuid,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
    pub input_unit_price_snapshot: String,
    pub output_unit_price_snapshot: String,
    pub user_amount: String,
    pub currency: String,
    pub usage_source: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: String,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantPaymentRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub amount: String,
    pub currency: String,
    pub status: PaymentState,
    pub payment_method: String,
    pub payment_scene: String,
    pub paid_at: Option<String>,
    pub closed_at: Option<String>,
    pub expired_at: String,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrencyTotals {
    pub currency: String,
    pub total_requests: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub total_amount: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageTotals {
    pub from: String,
    pub to: String,
    pub currencies: Vec<CurrencyTotals>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberWallet {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub available_balance: String,
    pub frozen_balance: String,
    pub total_recharged: String,
    pub total_consumed: String,
    pub initialized: bool,
    pub as_of: String,
}
fn invalid() -> ClientError {
    ClientError::InvalidResponse(
        "Financial report identity, values or pagination do not match the requested tenant scope"
            .into(),
    )
}
fn real(id: Uuid) -> Result<()> {
    if id.is_nil() {
        Err(ClientError::Config(
            "A real tenant or resource UUID is required".into(),
        ))
    } else {
        Ok(())
    }
}
/// Validate textual decimal syntax without parsing through floating point or rounding.
fn decimal(raw: &str) -> bool {
    if raw.is_empty() || raw.len() > 256 {
        return false;
    }
    let value = raw.strip_prefix('-').unwrap_or(raw);
    let (mantissa, exponent) = match value.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (value, None),
    };
    if exponent.is_some_and(|e| {
        e.parse::<i32>()
            .ok()
            .is_none_or(|n| !(-4096..=4096).contains(&n))
    }) {
        return false;
    }
    let mut dots = 0;
    let mut digits = 0;
    for b in mantissa.bytes() {
        if b.is_ascii_digit() {
            digits += 1;
        } else if b == b'.' {
            dots += 1;
        } else {
            return false;
        }
    }
    digits > 0 && dots <= 1
}
fn currency(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16 && !value.chars().any(char::is_control)
}
fn identity(
    tenant: Uuid,
    owner: Option<Uuid>,
    actual_tenant: Uuid,
    actual_owner: Uuid,
    id: Uuid,
) -> Result<()> {
    if tenant != actual_tenant
        || actual_owner.is_nil()
        || id.is_nil()
        || owner.is_some_and(|v| v != actual_owner)
    {
        return Err(invalid());
    }
    Ok(())
}
fn page<T>(q: &ReportQuery, p: &ReportPage<T>) -> Result<()> {
    if p.page != q.page
        || p.page_size != q.page_size
        || p.total < 0
        || p.total_pages < 0
        || p.items.len() > q.page_size as usize
        || p.items.len() as i64 > p.total
        || p.total_pages
            != p.total / i64::from(q.page_size) + i64::from(p.total % i64::from(q.page_size) != 0)
    {
        return Err(invalid());
    }
    Ok(())
}
#[derive(Debug, Clone)]
pub struct TenantReportingApi {
    client: ApiClient,
    tenant: Uuid,
    base: String,
}
impl TenantReportingApi {
    pub fn new(client: &ApiClient, tenant: Uuid) -> Result<Self> {
        real(tenant)?;
        Ok(Self {
            client: client.clone(),
            tenant,
            base: format!("/api/v1/tenants/{tenant}"),
        })
    }
    fn usage_check(&self, row: &TenantUsageRecord, owner: Option<Uuid>) -> Result<()> {
        identity(self.tenant, owner, row.tenant_id, row.user_id, row.id)?;
        if ![
            &row.input_unit_price_snapshot,
            &row.output_unit_price_snapshot,
            &row.user_amount,
        ]
        .into_iter()
        .all(|v| decimal(v))
            || !currency(&row.currency)
            || [row.input_tokens, row.output_tokens, row.total_tokens]
                .iter()
                .any(|n| *n < 0)
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn payment_check(&self, row: &TenantPaymentRecord, owner: Option<Uuid>) -> Result<()> {
        identity(self.tenant, owner, row.tenant_id, row.user_id, row.id)?;
        if !decimal(&row.amount) || !currency(&row.currency) {
            return Err(invalid());
        }
        Ok(())
    }
    pub async fn usage(
        &self,
        q: &ReportQuery,
        window: &ReportWindow,
        token: &str,
    ) -> Result<ReportPage<TenantUsageRecord>> {
        let r: ReportPage<TenantUsageRecord> = self
            .client
            .get_json_fresh(
                &format!(
                    "{}/billing/records?{}&{}",
                    self.base,
                    q.query()?,
                    window.query()?
                ),
                Some(token),
            )
            .await?;
        page(q, &r)?;
        let mut ids = HashSet::new();
        for row in &r.items {
            self.usage_check(row, q.owner_user_id)?;
            if !ids.insert(row.id) {
                return Err(invalid());
            }
        }
        Ok(r)
    }
    pub async fn usage_detail(
        &self,
        id: Uuid,
        owner: Uuid,
        token: &str,
    ) -> Result<TenantUsageRecord> {
        real(id)?;
        real(owner)?;
        let r: TenantUsageRecord = self
            .client
            .get_json_fresh(&format!("{}/billing/records/{id}", self.base), Some(token))
            .await?;
        self.usage_check(&r, Some(owner))?;
        if r.id != id {
            return Err(invalid());
        }
        Ok(r)
    }
    pub async fn totals(
        &self,
        window: &ReportWindow,
        owner: Option<Uuid>,
        token: &str,
    ) -> Result<UsageTotals> {
        if let Some(owner) = owner {
            real(owner)?;
        }
        let suffix = owner
            .map(|v| format!("&owner_user_id={v}"))
            .unwrap_or_default();
        let r: UsageTotals = self
            .client
            .get_json_fresh(
                &format!("{}/billing/stats?{}{suffix}", self.base, window.query()?),
                Some(token),
            )
            .await?;
        let mut seen = HashSet::new();
        for group in &r.currencies {
            if !currency(&group.currency)
                || !decimal(&group.total_amount)
                || !seen.insert(&group.currency)
                || [
                    group.total_requests,
                    group.total_input_tokens,
                    group.total_output_tokens,
                    group.total_tokens,
                ]
                .iter()
                .any(|v| *v < 0)
            {
                return Err(invalid());
            }
        }
        Ok(r)
    }
    pub async fn payments(
        &self,
        q: &ReportQuery,
        status: Option<PaymentState>,
        token: &str,
    ) -> Result<ReportPage<TenantPaymentRecord>> {
        let suffix = status
            .map(|v| format!("&status={}", v.as_str()))
            .unwrap_or_default();
        let r: ReportPage<TenantPaymentRecord> = self
            .client
            .get_json_fresh(
                &format!("{}/payments/orders?{}{suffix}", self.base, q.query()?),
                Some(token),
            )
            .await?;
        page(q, &r)?;
        let mut ids = HashSet::new();
        for row in &r.items {
            self.payment_check(row, q.owner_user_id)?;
            if !ids.insert(row.id) || status.is_some_and(|v| v != row.status) {
                return Err(invalid());
            }
        }
        Ok(r)
    }
    pub async fn payment(&self, id: Uuid, owner: Uuid, token: &str) -> Result<TenantPaymentRecord> {
        real(id)?;
        real(owner)?;
        let r: TenantPaymentRecord = self
            .client
            .get_json_fresh(&format!("{}/payments/orders/{id}", self.base), Some(token))
            .await?;
        self.payment_check(&r, Some(owner))?;
        if r.id != id {
            return Err(invalid());
        }
        Ok(r)
    }
    pub async fn wallet(&self, owner: Uuid, token: &str) -> Result<MemberWallet> {
        real(owner)?;
        let r: MemberWallet = self
            .client
            .get_json_fresh(&format!("{}/balances/{owner}", self.base), Some(token))
            .await?;
        if r.tenant_id != self.tenant
            || r.user_id != owner
            || r.as_of.is_empty()
            || ![
                &r.available_balance,
                &r.frozen_balance,
                &r.total_recharged,
                &r.total_consumed,
            ]
            .into_iter()
            .all(|v| decimal(v))
        {
            return Err(invalid());
        }
        Ok(r)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_report_numbers_never_need_a_float_or_a_currency_conversion() {
        for raw in [
            "0",
            "-0.0000000001",
            "9999999999.9999999999",
            "1E-10",
            "42.00",
            "1e+12",
        ] {
            assert!(decimal(raw), "{raw}");
        }
        for raw in [
            "NaN",
            "inf",
            "1.2.3",
            "1e999999",
            "",
            "-",
            "1e2e3",
            "1&tenant_id=x",
        ] {
            assert!(!decimal(raw), "{raw}");
        }
    }
}
