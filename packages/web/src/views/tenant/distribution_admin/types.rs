use chrono::{DateTime, Utc};
use client_api::{
    ClientError, Result,
    api::distribution_policy::{
        BeneficiaryScope, CreatePolicy, DistributionPolicy, PolicyPatch, validate_commission_rate,
    },
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub page: u32,
}
impl Default for Query {
    fn default() -> Self {
        Self { page: 1 }
    }
}

#[derive(Clone, PartialEq)]
pub enum Operation {
    Create,
    Edit(DistributionPolicy),
    Delete(DistributionPolicy),
    Default,
}
impl Operation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Create => "tenant_distribution.create",
            Self::Edit(_) => "tenant_distribution.edit",
            Self::Delete(_) => "tenant_distribution.delete",
            Self::Default => "tenant_distribution.default",
        }
    }
    pub fn row(&self) -> Option<&DistributionPolicy> {
        match self {
            Self::Edit(v) | Self::Delete(v) => Some(v),
            _ => None,
        }
    }
    pub fn key(&self) -> String {
        self.row()
            .map(|v| format!("{}:{}:{}", self.label(), v.id, v.updated_at))
            .unwrap_or_else(|| self.label().into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    pub description: String,
    pub rate: String,
    pub beneficiary_scope: BeneficiaryScope,
    pub beneficiary_id: String,
    pub priority: String,
    pub active: bool,
    pub from: String,
    pub until: String,
    pub reason: String,
}
impl Draft {
    pub fn for_operation(op: &Operation) -> Self {
        if let Some(v) = op.row() {
            Self {
                name: v.name.clone(),
                description: v.description.clone().unwrap_or_default(),
                rate: v.commission_rate.clone(),
                beneficiary_scope: v.beneficiary_scope,
                beneficiary_id: v.beneficiary_id.map(|v| v.to_string()).unwrap_or_default(),
                priority: v.priority.to_string(),
                active: v.is_active,
                from: v.effective_from.clone(),
                until: v.effective_until.clone().unwrap_or_default(),
                reason: String::new(),
            }
        } else {
            Self {
                name: String::new(),
                description: String::new(),
                rate: "0.1000".into(),
                beneficiary_scope: BeneficiaryScope::Everyone,
                beneficiary_id: String::new(),
                priority: "0".into(),
                active: true,
                from: String::new(),
                until: String::new(),
                reason: String::new(),
            }
        }
    }
    fn reason(&self) -> Result<String> {
        let v = self.reason.trim();
        if v.is_empty() || v.chars().count() > 500 || v.chars().any(char::is_control) {
            Err(ClientError::Config(
                "A bounded policy change reason is required".into(),
            ))
        } else {
            Ok(v.into())
        }
    }
    fn name(&self) -> Result<String> {
        let v = self.name.trim();
        if v.is_empty() || v.chars().count() > 255 || v.chars().any(char::is_control) {
            Err(ClientError::Config(
                "A bounded policy name is required".into(),
            ))
        } else {
            Ok(v.into())
        }
    }
    fn description(&self) -> Result<Option<String>> {
        let v = self.description.trim();
        if v.chars().count() > 4096 || v.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return Err(ClientError::Config(
                "Policy description is too long or invalid".into(),
            ));
        }
        Ok((!v.is_empty()).then(|| v.to_owned()))
    }
    fn priority(&self) -> Result<i32> {
        self.priority
            .trim()
            .parse::<i32>()
            .ok()
            .filter(|v| (-1000..=1000).contains(v))
            .ok_or_else(|| ClientError::Config("Policy priority must be within -1000..1000".into()))
    }
    fn date(raw: &str) -> Result<Option<String>> {
        let v = raw.trim();
        if v.is_empty() {
            return Ok(None);
        }
        DateTime::parse_from_rfc3339(v)
            .map_err(|_| ClientError::Config("Use RFC3339 timestamps with a timezone".into()))?;
        Ok(Some(v.into()))
    }
    fn window(&self) -> Result<(Option<String>, Option<String>)> {
        let from = Self::date(&self.from)?;
        let until = Self::date(&self.until)?;
        if let Some(end) = until.as_ref() {
            let end = DateTime::parse_from_rfc3339(end)
                .unwrap()
                .with_timezone(&Utc);
            let start = from
                .as_ref()
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                .map(|v| v.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            if end <= start {
                return Err(ClientError::Config(
                    "Policy end time must follow its start time".into(),
                ));
            }
        }
        Ok((from, until))
    }
    fn beneficiary(&self) -> Result<(BeneficiaryScope, Option<Uuid>)> {
        match self.beneficiary_scope {
            BeneficiaryScope::Everyone => Ok((BeneficiaryScope::Everyone, None)),
            BeneficiaryScope::TenantMember => {
                let id = Uuid::parse_str(self.beneficiary_id.trim())
                    .ok()
                    .filter(|v| !v.is_nil())
                    .ok_or_else(|| {
                        ClientError::Config("Select a real tenant member UUID".into())
                    })?;
                Ok((BeneficiaryScope::TenantMember, Some(id)))
            }
        }
    }
    pub fn create(&self) -> Result<CreatePolicy> {
        let name = self.name()?;
        let description = self.description()?;
        let rate = self.rate.trim().to_owned();
        validate_commission_rate(&rate)?;
        let (beneficiary_scope, beneficiary_id) = self.beneficiary()?;
        let priority = self.priority()?;
        let (effective_from, effective_until) = self.window()?;
        Ok(CreatePolicy {
            name,
            description,
            commission_rate: rate,
            beneficiary_scope,
            beneficiary_id,
            priority,
            effective_from,
            effective_until,
            reason: self.reason()?,
        })
    }
    pub fn patch(&self, original: &DistributionPolicy) -> Result<PolicyPatch> {
        let (scope, id) = self.beneficiary()?;
        if scope != original.beneficiary_scope || id != original.beneficiary_id {
            return Err(ClientError::Config(
                "Beneficiary identity is immutable; create a new policy instead".into(),
            ));
        }
        if original.updated_at.trim().is_empty() {
            return Err(ClientError::Config(
                "Reload the policy revision before editing".into(),
            ));
        }
        let rate = self.rate.trim().to_owned();
        validate_commission_rate(&rate)?;
        let (_, until) = self.window()?;
        let mut p = PolicyPatch::new(original.updated_at.clone(), self.reason()?);
        p.name = Some(self.name()?);
        p.description = Some(self.description()?);
        p.commission_rate = Some(rate);
        p.priority = Some(self.priority()?);
        p.is_active = Some(self.active);
        p.effective_until = Some(until);
        Ok(p)
    }
}
