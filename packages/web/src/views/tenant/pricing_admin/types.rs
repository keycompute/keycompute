use chrono::{DateTime, Utc};
use client_api::{
    ClientError, Result,
    api::tenant_pricing::{
        BillingDimension, CreateTenantPrice, TenantPrice, UpdateTenantPrice, validate_decimal,
    },
};
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub search: String,
    pub page: u32,
}
impl Default for Query {
    fn default() -> Self {
        Self {
            search: String::new(),
            page: 1,
        }
    }
}
#[derive(Clone, PartialEq)]
pub enum Operation {
    Create,
    Edit(TenantPrice),
    Delete(TenantPrice),
    Default(TenantPrice),
}
impl Operation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Create => "tenant_pricing.create",
            Self::Edit(_) => "tenant_pricing.edit",
            Self::Delete(_) => "tenant_pricing.delete",
            Self::Default(_) => "tenant_pricing.default",
        }
    }
    pub fn row(&self) -> Option<&TenantPrice> {
        match self {
            Self::Create => None,
            Self::Edit(v) | Self::Delete(v) | Self::Default(v) => Some(v),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub model: String,
    pub dimension: BillingDimension,
    pub currency: String,
    pub input: String,
    pub output: String,
    pub from: String,
    pub until: String,
    pub is_default: bool,
}
fn display_decimal(value: &str) -> String {
    value
        .parse::<Decimal>()
        .or_else(|_| Decimal::from_scientific(value))
        .map(|v| v.normalize().to_string())
        .unwrap_or_else(|_| value.to_owned())
}
impl Draft {
    pub fn for_operation(op: &Operation) -> Self {
        if let Some(v) = op.row() {
            Self {
                model: v.model_name.clone(),
                dimension: v.billing_dimension,
                currency: v.currency.clone(),
                input: display_decimal(&v.input_price_per_1k),
                output: display_decimal(&v.output_price_per_1k),
                from: v.effective_from.clone(),
                until: v.effective_until.clone().unwrap_or_default(),
                is_default: v.is_default,
            }
        } else {
            Self {
                model: String::new(),
                dimension: BillingDimension::ProviderAccount,
                currency: "CNY".into(),
                input: String::new(),
                output: String::new(),
                from: String::new(),
                until: String::new(),
                is_default: false,
            }
        }
    }
    fn prices(&self) -> Result<(String, String)> {
        let i = self.input.trim().to_owned();
        let o = self.output.trim().to_owned();
        validate_decimal(&i)?;
        validate_decimal(&o)?;
        Ok((i, o))
    }
    fn date(raw: &str) -> Result<Option<String>> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        DateTime::parse_from_rfc3339(raw)
            .map_err(|_| ClientError::Config("Use RFC3339 timestamps with a timezone".into()))?;
        Ok(Some(raw.into()))
    }
    fn window(&self) -> Result<(Option<String>, Option<String>)> {
        let from = Self::date(&self.from)?;
        let until = Self::date(&self.until)?;
        if let Some(end) = &until {
            let end = DateTime::parse_from_rfc3339(end)
                .map_err(|_| ClientError::Config("Invalid end time".into()))?
                .with_timezone(&Utc);
            let start = from
                .as_ref()
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                .map(|v| v.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            if end <= start {
                return Err(ClientError::Config(
                    "End time must follow start time".into(),
                ));
            }
        }
        Ok((from, until))
    }
    pub fn create(&self) -> Result<CreateTenantPrice> {
        let (input, output) = self.prices()?;
        let (effective_from, effective_until) = self.window()?;
        let model = self.model.trim();
        let currency = self.currency.trim().to_ascii_uppercase();
        if model.is_empty()
            || model.len() > 255
            || model.chars().any(char::is_control)
            || currency.len() != 3
            || !currency.bytes().all(|v| v.is_ascii_uppercase())
        {
            return Err(ClientError::Config(
                "Invalid pricing model or currency".into(),
            ));
        }
        Ok(CreateTenantPrice {
            model_name: model.into(),
            billing_dimension: self.dimension,
            currency,
            input_price_per_1k: input,
            output_price_per_1k: output,
            is_default: self.is_default,
            effective_from,
            effective_until,
        })
    }
    pub fn update(&self, original: &TenantPrice) -> Result<UpdateTenantPrice> {
        if original.version <= 0
            || self.model != original.model_name
            || self.dimension != original.billing_dimension
            || self.currency != original.currency
        {
            return Err(ClientError::Config(
                "Reload the original pricing identity and version".into(),
            ));
        }
        let (input, output) = self.prices()?;
        let (_, until) = self.window()?;
        Ok(UpdateTenantPrice {
            expected_version: original.version,
            input_price_per_1k: Some(input),
            output_price_per_1k: Some(output),
            effective_until: until,
        })
    }
}
