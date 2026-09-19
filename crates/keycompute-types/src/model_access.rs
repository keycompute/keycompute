//! Product-facing access modes. Protocol, credential, selection provenance and
//! execution resource remain separate concepts; these contracts explain the
//! configured model offering without disclosing upstream secrets.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAccessMode {
    #[default]
    AccountPool,
    Passthrough,
    NodeDispatch,
}
impl ModelAccessMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccountPool => "account_pool",
            Self::Passthrough => "passthrough",
            Self::NodeDispatch => "node_dispatch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    /// Eligible at the observation instant; not a capacity reservation.
    Ready,
    Disabled,
    Unverified,
    Stale,
    Unhealthy,
    Unavailable,
}

/// Administrator-only model offering; no endpoint or upstream credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub model: String,
    pub request_model: String,
    pub protocol: String,
    pub capability: String,
    pub request_path: String,
    pub status: ModelAvailability,
    pub reason_code: String,
    pub configured_targets: u64,
    pub eligible_targets: u64,
    pub binding_id: Option<String>,
    pub binding_revision: Option<i64>,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub pool_enabled: Option<bool>,
    pub health_expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogPage {
    pub mode: ModelAccessMode,
    pub tenant_id: String,
    pub entries: Vec<ModelCatalogEntry>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
    pub total_pages: u64,
    pub observed_at: String,
}

/// Bounded, tenant-authorized account choices for the passthrough setup flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingAccountOption {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub pool_enabled: bool,
    pub models: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingAccountOptions {
    pub accounts: Vec<BindingAccountOption>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn product_modes_are_distinct_from_protocol_and_execution_provenance() {
        for (value, name) in [
            (ModelAccessMode::AccountPool, "account_pool"),
            (ModelAccessMode::Passthrough, "passthrough"),
            (ModelAccessMode::NodeDispatch, "node_dispatch"),
        ] {
            assert_eq!(value.as_str(), name);
            assert_eq!(serde_json::to_value(value).unwrap(), name);
            assert_eq!(
                serde_json::from_value::<ModelAccessMode>(name.into()).unwrap(),
                value
            );
        }
        assert!(serde_json::from_str::<ModelAccessMode>("\"openai\"").is_err());
        assert!(serde_json::from_str::<ModelAccessMode>("\"node_token\"").is_err());
    }
}
