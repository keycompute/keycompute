use chrono::{DateTime, Duration, SecondsFormat, Utc};
use client_api::{
    ClientError, Result,
    api::tenant_reporting::{
        PaymentState, ReportQuery, ReportWindow, TenantPaymentRecord, TenantUsageRecord,
    },
};
use uuid::Uuid;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Usage,
    Orders,
    Wallet,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub tab: Tab,
    pub owner: Option<Uuid>,
    pub window: ReportWindow,
    pub status: Option<PaymentState>,
    pub page: u32,
}
impl Default for Query {
    fn default() -> Self {
        let to = Utc::now();
        let from = to - Duration::days(30);
        Self {
            tab: Tab::Usage,
            owner: None,
            window: ReportWindow {
                from: from.to_rfc3339_opts(SecondsFormat::Secs, true),
                to: to.to_rfc3339_opts(SecondsFormat::Secs, true),
            },
            status: None,
            page: 1,
        }
    }
}
impl Query {
    pub fn report(&self) -> ReportQuery {
        ReportQuery {
            page: self.page,
            page_size: 20,
            owner_user_id: self.owner,
        }
    }
}
pub fn owner(raw: &str) -> Result<Option<Uuid>> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    raw.trim()
        .parse::<Uuid>()
        .ok()
        .filter(|v| !v.is_nil())
        .map(Some)
        .ok_or_else(|| ClientError::Config("Select a real member UUID".into()))
}
pub fn window(from: &str, to: &str) -> Result<ReportWindow> {
    let parse = |s: &str| {
        DateTime::parse_from_rfc3339(s.trim())
            .map(|v| v.with_timezone(&Utc))
            .map_err(|_| ClientError::Config("Use RFC3339 dates with a timezone".into()))
    };
    let from = parse(from)?;
    let to = parse(to)?;
    if from >= to || to - from > Duration::days(31) {
        return Err(ClientError::Config(
            "Choose a start before end and at most 31 days per report".into(),
        ));
    }
    Ok(ReportWindow {
        from: from.to_rfc3339(),
        to: to.to_rfc3339(),
    })
}
pub fn status(raw: &str) -> Result<Option<PaymentState>> {
    match raw {
        "" => Ok(None),
        "pending" => Ok(Some(PaymentState::Pending)),
        "paid" => Ok(Some(PaymentState::Paid)),
        "failed" => Ok(Some(PaymentState::Failed)),
        "closed" => Ok(Some(PaymentState::Closed)),
        _ => Err(ClientError::Config("Unknown payment status".into())),
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detail {
    Usage(TenantUsageRecord),
    Order(TenantPaymentRecord),
}
impl Detail {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Usage(r) => r.id,
            Self::Order(r) => r.id,
        }
    }
    pub fn owner(&self) -> Uuid {
        match self {
            Self::Usage(r) => r.user_id,
            Self::Order(r) => r.user_id,
        }
    }
    pub fn key(&self) -> String {
        match self {
            Self::Usage(r) => {
                serde_json::json!(["usage", r.tenant_id, r.user_id, r.id]).to_string()
            }
            Self::Order(r) => {
                serde_json::json!(["order", r.tenant_id, r.user_id, r.id, r.updated_at]).to_string()
            }
        }
    }
}
